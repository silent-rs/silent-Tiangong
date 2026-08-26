//! 插件发布清单数字签名与制品完整性验证。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{MANIFEST_FILE, PluginManifest, SidecarRuntime};
use crate::sidecar::CONTENT_MANIFEST_FILE;

pub const SIGNED_RELEASE_FILE: &str = "release.json";
pub const SIGNATURE_FILE: &str = "release.json.sig";
pub const SIGNED_RELEASE_SCHEMA_VERSION: u32 = 1;

/// 插件专用 minisign 公钥。私钥只保存在官方发布环境（CI secret 与
/// 官方开发者本机，打包时经 TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH 提供）。
const OFFICIAL_PLUGIN_PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDkwQzBDOEJEQ0IzRTI5OTgKUldTWUtUN0x2Y2pBa0piU3JNQi9VRDlENVdxNzd6S3Z1MGo1ck5Sd2ZwNTRKTnpVTGkyWjE5dGMK";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPluginRelease {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub publisher: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub manifest: SignedArtifact,
    /// 纯 TS/sidecar 插件无 wasm 制品，签名清单省略 wasm 条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<SignedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ui: Vec<SignedArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<SignedArtifact>,
    /// 解释器 sidecar 插件：官方签名锚定完整内容清单（content-manifest.json
    /// 的哈希），清单覆盖全树——与本地信任锚同构，签名即信任根。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_manifest: Option<SignedArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedArtifact {
    pub path: PathBuf,
    pub sha256: String,
}

impl SignedPluginRelease {
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|item| item == permission)
    }
}

/// 验证签名发布清单及其覆盖的当前平台制品。
///
/// 缺少签名文件返回 `Ok(None)`，表示普通未签名插件；任何签名或制品不一致均返回错误。
pub fn verify_signed_release(
    directory: &Path,
    plugin_manifest: &PluginManifest,
) -> Result<Option<SignedPluginRelease>> {
    let release_path = directory.join(SIGNED_RELEASE_FILE);
    let signature_path = directory.join(SIGNATURE_FILE);
    let release_exists = release_path.is_file();
    let signature_exists = signature_path.is_file();
    if !release_exists && !signature_exists {
        return Ok(None);
    }
    if !release_exists || !signature_exists {
        bail!("插件签名文件不完整");
    }

    let release_bytes = std::fs::read(&release_path)
        .with_context(|| format!("读取插件签名清单失败: {}", release_path.display()))?;
    verify_minisign(&release_bytes, &std::fs::read_to_string(&signature_path)?)?;
    let release: SignedPluginRelease =
        serde_json::from_slice(&release_bytes).context("解析插件签名清单失败")?;
    release.validate(directory, plugin_manifest)?;
    Ok(Some(release))
}

impl SignedPluginRelease {
    fn validate(&self, directory: &Path, plugin_manifest: &PluginManifest) -> Result<()> {
        if self.schema_version != SIGNED_RELEASE_SCHEMA_VERSION {
            bail!("插件签名清单版本不支持: {}", self.schema_version);
        }
        if self.publisher != "tiangong-official" {
            bail!("插件签名发布者无效: {}", self.publisher);
        }
        if self.id != plugin_manifest.id || self.version != plugin_manifest.version {
            bail!("插件签名清单与 plugin.json 的 ID 或版本不一致");
        }
        if self.permissions.iter().any(|item| item.trim().is_empty())
            || permission_set(&self.permissions)?.len() != self.permissions.len()
        {
            bail!("插件签名清单包含空权限或重复权限");
        }
        if plugin_manifest
            .permissions
            .iter()
            .any(|item| item.trim().is_empty())
            || permission_set(&plugin_manifest.permissions)?.len()
                != plugin_manifest.permissions.len()
        {
            bail!("plugin.json 包含空权限或重复权限");
        }
        if permission_set(&self.permissions)? != permission_set(&plugin_manifest.permissions)? {
            bail!("插件签名清单与 plugin.json 的权限声明不一致");
        }
        self.manifest.verify(directory, Path::new(MANIFEST_FILE))?;
        // wasm 条目与 plugin.json 声明一致才校验；纯 TS/sidecar 插件两边都缺省。
        match (&self.wasm, plugin_manifest.wasm_binary()) {
            (Some(signed_wasm), Some(wasm_binary)) => signed_wasm.verify(directory, wasm_binary)?,
            (None, None) => {}
            _ => bail!("插件签名清单与 plugin.json 的 wasm 声明不一致"),
        }
        let expected_ui = plugin_manifest
            .ui_contributions()
            .into_iter()
            .map(|contribution| PathBuf::from(contribution.entry))
            .collect::<BTreeSet<_>>();
        let signed_ui = self
            .ui
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect::<BTreeSet<_>>();
        if signed_ui.len() != self.ui.len() || signed_ui != expected_ui {
            bail!("插件签名清单与 plugin.json 的 UI 入口声明不一致");
        }
        for artifact in &self.ui {
            artifact.verify(directory, &artifact.path)?;
        }
        // 插件声明 sidecar 时，官方签名必须同时授权调用（两种形态一致）。
        if plugin_manifest.sidecar.is_some() && !self.has_permission("sidecar.invoke") {
            bail!("插件签名清单未授权 sidecar.invoke");
        }
        match (&self.sidecar, &plugin_manifest.sidecar) {
            (Some(signed), Some(sidecar)) => match sidecar.runtime {
                SidecarRuntime::Native => {
                    if self.content_manifest.is_some() {
                        bail!("native sidecar 签名清单不得包含 content_manifest 条目");
                    }
                    let binary = sidecar.binary.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "插件 {} native sidecar 缺少 binary 声明",
                            plugin_manifest.id
                        )
                    })?;
                    signed.verify(directory, &sidecar_binary_path(binary)?)?;
                }
                SidecarRuntime::Node | SidecarRuntime::Python => {
                    bail!(
                        "解释器 sidecar 签名清单不得包含二进制条目（信任由 content_manifest 承载）"
                    );
                }
            },
            (None, None) => {}
            // 解释器官方形态：签名无二进制条目，信任锚是完整内容清单——
            // 先校验清单文件哈希与签名一致，再按清单双向校验全树。
            (None, Some(sidecar)) if sidecar.runtime != SidecarRuntime::Native => {
                let signed_manifest = self.content_manifest.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("官方解释器插件缺少 content_manifest 签名条目")
                })?;
                signed_manifest.verify(directory, Path::new(CONTENT_MANIFEST_FILE))?;
                crate::sidecar::SidecarConfig::verify_integrity_manifest(
                    &directory.join(CONTENT_MANIFEST_FILE),
                    directory,
                )?;
            }
            _ => bail!("插件签名清单与 plugin.json 的 sidecar 声明不一致"),
        }
        // 内容清单条目只属于解释器形态：无 sidecar 声明或 native 形态携带该条目
        // 均不符合发布约定。
        if self.content_manifest.is_some()
            && plugin_manifest
                .sidecar
                .as_ref()
                .is_none_or(|sidecar| sidecar.runtime == SidecarRuntime::Native)
        {
            bail!("content_manifest 签名条目仅允许解释器 sidecar 形态使用");
        }
        Ok(())
    }
}

impl SignedArtifact {
    fn verify(&self, directory: &Path, expected_path: &Path) -> Result<()> {
        validate_relative_path(&self.path)?;
        if self.path != expected_path {
            bail!(
                "签名制品路径不一致: expected={}, actual={}",
                expected_path.display(),
                self.path.display()
            );
        }
        let actual = sha256_file(&directory.join(&self.path))?;
        if !actual.eq_ignore_ascii_case(self.sha256.trim_start_matches("sha256:")) {
            bail!("签名制品校验失败: {}", self.path.display());
        }
        Ok(())
    }
}

fn permission_set(permissions: &[String]) -> Result<BTreeSet<&str>> {
    Ok(permissions.iter().map(String::as_str).collect())
}

fn verify_minisign(content: &[u8], signature_b64: &str) -> Result<()> {
    // 公钥可被环境变量覆盖（CI/本地端到端验证用；不改变内置官方公钥）
    let pubkey_b64 = std::env::var("TIANGONG_PLUGIN_PUBKEY_B64")
        .unwrap_or_else(|_| OFFICIAL_PLUGIN_PUBKEY_B64.to_string());
    let public_text = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64)
        .context("解析内置插件公钥失败")?;
    let public_text = String::from_utf8(public_text).context("内置插件公钥非 UTF-8")?;
    let public_key = PublicKey::decode(&public_text).context("解析插件公钥失败")?;
    let signature_text = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .context("解析插件签名失败")?;
    let signature_text = String::from_utf8(signature_text).context("插件签名非 UTF-8")?;
    let signature = Signature::decode(&signature_text).context("解析 minisign 插件签名失败")?;
    public_key
        .verify(content, &signature, false)
        .context("插件签名验证不通过")
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("读取签名制品失败: {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("签名制品路径必须是安全相对路径");
    }
    Ok(())
}

fn sidecar_binary_path(path: &Path) -> Result<PathBuf> {
    let mut path = path.to_path_buf();
    if !std::env::consts::EXE_SUFFIX.is_empty() {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .context("sidecar 文件名无效")?;
        if !name.ends_with(std::env::consts::EXE_SUFFIX) {
            path.set_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::PluginManifest;

    /// devkit 同款内容清单：受管全树（排除清单自身与签名文件）路径 + sha256。
    fn write_content_manifest(dir: &Path) {
        let mut files = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                let path = entry.path();
                let relative = path.strip_prefix(dir).unwrap().display().to_string();
                if relative == CONTENT_MANIFEST_FILE
                    || relative == SIGNED_RELEASE_FILE
                    || relative == SIGNATURE_FILE
                {
                    continue;
                }
                let raw = std::fs::read(&path).unwrap();
                files.push(serde_json::json!({
                    "path": relative.replace('\\', "/"),
                    "sha256": hex::encode(Sha256::digest(&raw)),
                }));
            }
        }
        std::fs::write(
            dir.join(CONTENT_MANIFEST_FILE),
            serde_json::to_vec(&serde_json::json!({"algorithm": "sha256", "files": files}))
                .unwrap(),
        )
        .unwrap();
    }

    /// 官方解释器插件目录：plugin.json + UI 入口 + node sidecar 入口 + 模板
    /// 资源 + 内容清单。
    fn setup_official_interpreter_plugin(root: &Path) -> (PathBuf, PluginManifest) {
        let dir = root.join("plugins").join("official-demo");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::create_dir_all(dir.join("sidecar")).unwrap();
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"official-demo","version":"0.2.0","permissions":["sidecar.invoke"],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]},"sidecar":{"runtime":"node","entry":"sidecar/main.mjs"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html>demo</html>").unwrap();
        std::fs::write(dir.join("sidecar/main.mjs"), "// sidecar bundle").unwrap();
        std::fs::write(dir.join("templates/node-tool.txt"), "template").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).expect("清单合法");
        (dir, manifest)
    }

    fn artifact(dir: &Path, path: &str) -> SignedArtifact {
        SignedArtifact {
            path: PathBuf::from(path),
            sha256: sha256_file(&dir.join(path)).unwrap(),
        }
    }

    /// 生成密钥对、按回调调整签名清单、落盘 release.json 与签名；返回需注入
    /// 环境的测试公钥（verify_minisign 读 TIANGONG_PLUGIN_PUBKEY_B64）。
    fn sign_official_release(
        dir: &Path,
        manifest: &PluginManifest,
        mutate: impl FnOnce(&mut SignedPluginRelease),
    ) -> String {
        let mut release = SignedPluginRelease {
            schema_version: SIGNED_RELEASE_SCHEMA_VERSION,
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            publisher: "tiangong-official".to_string(),
            permissions: manifest.permissions.clone(),
            manifest: artifact(dir, MANIFEST_FILE),
            wasm: None,
            ui: manifest
                .ui_contributions()
                .into_iter()
                .map(|contribution| artifact(dir, &contribution.entry))
                .collect(),
            sidecar: None,
            content_manifest: Some(artifact(dir, CONTENT_MANIFEST_FILE)),
        };
        mutate(&mut release);
        let bytes = serde_json::to_vec_pretty(&release).unwrap();
        std::fs::write(dir.join(SIGNED_RELEASE_FILE), &bytes).unwrap();
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature =
            minisign::sign(Some(&keypair.pk), &keypair.sk, bytes.as_slice(), None, None).unwrap();
        // 与 tauri signer 输出一致：两行签名文本整体 base64 包装（verify_minisign 读取格式）。
        let signature_text = signature.into_string();
        std::fs::write(
            dir.join(SIGNATURE_FILE),
            base64::engine::general_purpose::STANDARD.encode(signature_text),
        )
        .unwrap();
        base64::engine::general_purpose::STANDARD.encode(keypair.pk.to_box().unwrap().into_string())
    }

    fn run_with_pubkey<F: FnOnce()>(pubkey_b64: &str, body: F) {
        let previous = std::env::var("TIANGONG_PLUGIN_PUBKEY_B64").ok();
        unsafe {
            std::env::set_var("TIANGONG_PLUGIN_PUBKEY_B64", pubkey_b64);
        }
        body();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("TIANGONG_PLUGIN_PUBKEY_B64", value),
                None => std::env::remove_var("TIANGONG_PLUGIN_PUBKEY_B64"),
            }
        }
    }

    fn assert_verify_ok(dir: &Path, manifest: &PluginManifest) {
        verify_signed_release(dir, manifest).expect("官方签名验证应通过");
    }

    fn assert_verify_err(dir: &Path, manifest: &PluginManifest, needle: &str) {
        let error = verify_signed_release(dir, manifest).expect_err("官方签名验证应被拒绝");
        assert!(error.to_string().contains(needle), "{error:#}");
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_验签通过并覆盖全树() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |_| {});
        run_with_pubkey(&key, || assert_verify_ok(&dir, &manifest));
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_缺少内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |release| {
            release.content_manifest = None;
        });
        run_with_pubkey(&key, || {
            assert_verify_err(&dir, &manifest, "content_manifest")
        });
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_内容清单条目路径不是固定文件名拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |release| {
            release.content_manifest.as_mut().unwrap().path = PathBuf::from("sidecar/main.mjs");
        });
        run_with_pubkey(&key, || {
            assert_verify_err(&dir, &manifest, "签名制品路径不一致")
        });
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_篡改内容清单拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |_| {});
        std::fs::write(dir.join(CONTENT_MANIFEST_FILE), "{}").unwrap();
        run_with_pubkey(&key, || assert_verify_err(&dir, &manifest, "校验失败"));
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_篡改sidecar入口拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |_| {});
        std::fs::write(dir.join("sidecar/main.mjs"), "// tampered").unwrap();
        run_with_pubkey(&key, || assert_verify_err(&dir, &manifest, "不一致"));
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_篡改模板资源拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |_| {});
        std::fs::write(dir.join("templates/node-tool.txt"), "tampered").unwrap();
        run_with_pubkey(&key, || assert_verify_err(&dir, &manifest, "不一致"));
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_添加未列出文件拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |_| {});
        std::fs::write(dir.join("extra-payload.mjs"), "unknown file").unwrap();
        run_with_pubkey(&key, || assert_verify_err(&dir, &manifest, "未覆盖"));
    }

    #[test]
    #[serial_test::serial]
    fn 官方解释器插件_签名携带sidecar二进制条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let (dir, manifest) = setup_official_interpreter_plugin(root.path());
        let key = sign_official_release(&dir, &manifest, |release| {
            release.sidecar = Some(artifact(&dir, "sidecar/main.mjs"));
        });
        run_with_pubkey(&key, || assert_verify_err(&dir, &manifest, "二进制条目"));
    }

    #[test]
    #[serial_test::serial]
    fn 官方native插件_携带内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugins").join("native-demo");
        std::fs::create_dir_all(dir.join("sidecar")).unwrap();
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"native-demo","version":"0.2.0","permissions":["sidecar.invoke"],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]},"sidecar":{"runtime":"native","binary":"sidecar/demo-bin"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html></html>").unwrap();
        std::fs::write(dir.join("sidecar/demo-bin"), b"fake native binary").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).unwrap();
        let key = sign_official_release(&dir, &manifest, |release| {
            release.sidecar = Some(artifact(&dir, "sidecar/demo-bin"));
        });
        run_with_pubkey(&key, || {
            assert_verify_err(&dir, &manifest, "content_manifest 条目")
        });
    }

    #[test]
    #[serial_test::serial]
    fn 纯ui插件_携带内容清单条目拒绝() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("plugins").join("ui-demo");
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"schema_version":2,"id":"ui-demo","version":"0.2.0","permissions":[],"ui":{"contributions":[{"slot":"extension.tab","id":"app","entry":"dist/index.html"}]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("dist/index.html"), "<html></html>").unwrap();
        write_content_manifest(&dir);
        let manifest = PluginManifest::load(&dir.join(MANIFEST_FILE)).unwrap();
        let key = sign_official_release(&dir, &manifest, |_| {});
        run_with_pubkey(&key, || {
            assert_verify_err(&dir, &manifest, "仅允许解释器 sidecar 形态")
        });
    }
}
