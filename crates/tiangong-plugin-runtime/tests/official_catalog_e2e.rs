//! 官方目录解释器插件安装全链路（显式环境门控）：目录发现 → 归档下载 →
//! 安全解包（含归档内外 plugin.json 一致性比对）→ 官方签名验签（内容清单
//! 全树校验）→ 原子安装 → 注册表加载 → sidecar 真实调用 → 卸载。
//!
//! 触发条件（产物与公钥均就绪才运行；两者由环境提供）；设置
//! `TIANGONG_PLUGIN_E2E_REQUIRED=1` 后进入 fail-closed 模式——缺产物或
//! 公钥直接失败而非跳过（CI 使用，防止工作流前置断言缺失时假绿灯）：
//! - `TIANGONG_PLUGIN_E2E_DIST`：发布产物目录（默认 workspace 的
//!   `target/plugin-dist`，先经 `cargo run -p xtask -- build-plugin
//!   plugin-creator` 生成）；
//! - `TIANGONG_PLUGIN_E2E_PUBKEY_B64`：与签名私钥对应的测试公钥，
//!   即 base64(minisign 公钥文本)——`generate-plugin-test-key` 产物的
//!   `.pub` 文件内容（单行 base64，直接作为该环境变量的值）。
//!
//! CI 在运行前先断言两者存在（保证发布链每次必跑）；本地默认无产物时跳过。

use std::path::{Path, PathBuf};

fn workspace_target_dist() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/plugin-dist")
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

/// 官方目录安装全链路：本地 http 目录服务承载真实发布产物，完整走
/// 下载/解包/验签/安装/调用。产物目录复制到临时目录并重写 URL——不
/// 改写共享构建产物，测试互不污染。
#[test]
#[serial_test::serial]
fn 官方目录_解释器插件安装全链路() {
    let dist = std::env::var_os("TIANGONG_PLUGIN_E2E_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(workspace_target_dist);
    let pubkey_b64 = std::env::var("TIANGONG_PLUGIN_E2E_PUBKEY_B64")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let fragment = dist.join("plugins-index/fragments/plugin-creator-any.json");
    let archive = dist.join("plugins/plugin-creator/0.2.0/plugin-creator-0.2.0.tar.zst");
    if !fragment.is_file() || !archive.is_file() || pubkey_b64.is_none() {
        let reason = format!(
            "缺少发布产物（fragment={} archive={}）或测试公钥（pubkey={}）",
            fragment.display(),
            archive.display(),
            pubkey_b64.is_some()
        );
        assert!(
            std::env::var_os("TIANGONG_PLUGIN_E2E_REQUIRED").is_none(),
            "E2E fail-closed 模式下不得跳过：{reason}"
        );
        eprintln!("跳过：{reason}（CI 设 TIANGONG_PLUGIN_E2E_REQUIRED=1 强制必跑）");
        return;
    }

    // 产物副本（重写 catalog/fragment 中的 OSS URL 到本地服务）。
    let dist_copy = tempfile::tempdir().unwrap();
    copy_tree(&dist, dist_copy.path());
    let fragment = dist_copy
        .path()
        .join("plugins-index/fragments/plugin-creator-any.json");
    let catalog_path = dist_copy.path().join("plugins-index/catalog.json");

    // 本地目录服务（随机端口）。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut server = std::process::Command::new("python3")
        .args(["-m", "http.server", &port.to_string()])
        .current_dir(dist_copy.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("启动本地目录服务失败");
    std::thread::sleep(std::time::Duration::from_millis(500));

    let previous_catalog = std::env::var("TIANGONG_PLUGIN_CATALOG_URL").ok();
    let previous_pubkey = std::env::var("TIANGONG_PLUGIN_PUBKEY_B64").ok();
    let catalog_url = format!("http://127.0.0.1:{port}/plugins-index/catalog.json");
    unsafe {
        std::env::set_var("TIANGONG_PLUGIN_CATALOG_URL", &catalog_url);
        std::env::set_var(
            "TIANGONG_PLUGIN_PUBKEY_B64",
            pubkey_b64.expect("上方已校验非空"),
        );
    }
    let rewrite_oss_urls = |path: &Path| {
        let raw = std::fs::read_to_string(path).unwrap();
        std::fs::write(
            path,
            raw.replace(
                "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com",
                &format!("http://127.0.0.1:{port}"),
            ),
        )
        .unwrap();
    };
    rewrite_oss_urls(&fragment);
    rewrite_oss_urls(&catalog_path);

    let storage = tempfile::tempdir().unwrap();
    tiangong_config::registry::init_from_dir(&storage.path().join("config"));

    let result = std::panic::catch_unwind(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let repository =
                    tiangong_plugin_runtime::artifacts::PluginRepository::new().expect("构造下载器");
                let staged = repository
                    .download(storage.path(), "plugin-creator", None)
                    .await
                    .expect("目录发现与归档下载解包");
                let status = tiangong_plugin_runtime::registry::install_staged_plugin(
                    storage.path(),
                    staged.path(),
                )
                .expect("官方签名验签与安装");
                assert_eq!(status.manifest_version, "0.2.0");
                // 正式发现 + sidecar 真实调用（官方签名解释器放行路径）。
                tiangong_plugin_runtime::registry::preload_installed_plugins(storage.path());
                let response = tiangong_plugin_runtime::registry::invoke_sidecar(
                    storage.path(),
                    "plugin-creator",
                    "devkit.validate",
                    serde_json::json!({"args": ["nonexistent"], "root": "/tmp/catalog-install-dev"}),
                )
                .expect("官方签名解释器 sidecar 调用");
                assert_eq!(
                    response["ok"],
                    serde_json::json!(false),
                    "探针项目应校验失败（证明真实执行）"
                );
            });
    });
    // 恢复现场。
    unsafe {
        match previous_catalog {
            Some(value) => std::env::set_var("TIANGONG_PLUGIN_CATALOG_URL", value),
            None => std::env::remove_var("TIANGONG_PLUGIN_CATALOG_URL"),
        }
        match previous_pubkey {
            Some(value) => std::env::set_var("TIANGONG_PLUGIN_PUBKEY_B64", value),
            None => std::env::remove_var("TIANGONG_PLUGIN_PUBKEY_B64"),
        }
    }
    let _ = server.kill();
    let _ = server.wait();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

/// 卸载语义：目录安装 → 卸载 → 目录移除且无可选插件被强制装回的通道
/// （无内置部署是结构性保证，本测试固化该语义）。
///
/// 安装源优先取发布归档（解包出受管文件树，CI 与全链路测试同源）；本地
/// 开发目录存在 yarn package 产物时亦可作为源（两者皆缺则跳过）。
#[test]
#[serial_test::serial]
fn 官方目录_卸载后不自动恢复() {
    let storage = tempfile::tempdir().unwrap();
    tiangong_config::registry::init_from_dir(&storage.path().join("config"));

    // 安装源：优先发布归档，其次本地 yarn package 产物。
    let dist = std::env::var_os("TIANGONG_PLUGIN_E2E_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(workspace_target_dist);
    let release_copy = storage.path().join("plugins-dev/plugin-creator/release");
    let archive = dist.join("plugins/plugin-creator/0.2.0/plugin-creator-0.2.0.tar.zst");
    let local_release = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/tiangong-plugin-creator/release");
    if archive.is_file() {
        std::fs::create_dir_all(&release_copy).unwrap();
        tiangong_plugin_runtime::artifacts::extract_plugin_archive(&archive, &release_copy)
            .expect("解包发布归档");
    } else if local_release.join("plugin.json").is_file() {
        copy_tree(&local_release, &release_copy);
    } else {
        assert!(
            std::env::var_os("TIANGONG_PLUGIN_E2E_REQUIRED").is_none(),
            "E2E fail-closed 模式下不得跳过：缺少发布归档与 creator release 产物"
        );
        eprintln!("跳过：缺少发布归档或 creator release 产物");
        return;
    }
    let staged =
        tiangong_plugin_runtime::artifacts::stage_local_plugin(storage.path(), &release_copy)
            .expect("暂存");
    let manifest_raw = std::fs::read(staged.path().join("content-manifest.json")).unwrap();
    use sha2::Digest;
    let anchor = hex::encode(sha2::Sha256::digest(&manifest_raw));
    std::fs::write(
        staged.path().join("local-trust.json"),
        format!(r#"{{"kind":"local-confirm","content_sha256":"{anchor}"}}"#),
    )
    .unwrap();
    let status =
        tiangong_plugin_runtime::registry::import_staged_plugin(storage.path(), staged.path())
            .expect("安装");
    assert!(
        storage
            .path()
            .join("plugins/plugin-creator/plugin.json")
            .is_file()
    );

    // 正规卸载 API：目录移除 + 注册表清理。
    tiangong_plugin_runtime::registry::uninstall_plugin(storage.path(), "plugin-creator", false)
        .expect("卸载");
    assert!(
        !storage
            .path()
            .join("plugins/plugin-creator/plugin.json")
            .is_file(),
        "卸载后插件目录应移除"
    );
    // 全新进程的发现语义：插件目录不存在则不会被发现；本应用无内置部署
    // 通道（结构性保证），不存在卸载后自动恢复的路径。
    assert_eq!(status.manifest_version, "0.2.0");
}
