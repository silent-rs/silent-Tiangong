use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, anyhow};
use semver::Version;
use serde::Deserialize;

use crate::args::UpdateArgs;

const UPDATE_ENDPOINTS: &[&str] = &[
    "https://github.com/silent-rs/silent-Tiangong/releases/latest/download/latest.json",
    "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com/latest.json",
];

#[derive(Debug, Deserialize)]
struct ReleaseManifest {
    #[serde(alias = "name")]
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    platforms: BTreeMap<String, ReleasePlatform>,
}

#[derive(Debug, Deserialize)]
struct ReleasePlatform {
    url: String,
    #[allow(dead_code)]
    signature: String,
}

pub(crate) fn run_update_command(args: UpdateArgs) -> anyhow::Result<()> {
    let endpoints: Vec<&str> = if let Some(custom) = args
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        vec![custom]
    } else {
        UPDATE_ENDPOINTS.to_vec()
    };

    let mut manifest = None;
    let mut last_error = None;
    for endpoint in &endpoints {
        match fetch_release_manifest(endpoint) {
            Ok(Some(m)) => {
                manifest = Some(m);
                break;
            }
            Ok(None) => {
                // 404 — 没有发布，继续尝试下一个
                last_error = None;
            }
            Err(e) => {
                last_error = Some(e);
            }
        }
    }

    if let Some(err) = last_error
        && manifest.is_none()
    {
        return Err(err);
    }

    let Some(manifest) = manifest else {
        println!("当前没有可用的在线更新发布。");
        return Ok(());
    };
    let current_version = parse_version(env!("CARGO_PKG_VERSION"))?;
    let latest_version = parse_version(&manifest.version)?;

    println!("当前版本：{}", current_version);
    println!("最新版本：{}", latest_version);

    if latest_version <= current_version {
        println!("当前已是最新版本。");
        return Ok(());
    }

    println!("发现新版本：{}", manifest.version.trim());
    if let Some(notes) = manifest
        .notes
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        println!("\n更新说明：\n{}", notes.trim());
    }

    if let Some(platform) = current_platform_key().and_then(|key| manifest.platforms.get(&key)) {
        println!("\n更新包：{}", platform.url);
    }

    if args.check {
        return Ok(());
    }

    println!("\n当前命令行入口已完成在线更新检查。");
    println!(
        "如需自动下载并安装，请使用已安装的桌面应用二进制运行 `tiangong update`，或在桌面应用设置中点击更新。"
    );
    Ok(())
}

fn fetch_release_manifest(endpoint: &str) -> anyhow::Result<Option<ReleaseManifest>> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("tiangong-cli-updater")
        .build()
        .context("初始化更新客户端失败")?
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .with_context(|| format!("请求更新源失败：{endpoint}"))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(anyhow!("更新源返回异常状态：{}", response.status()));
    }

    response.json().map(Some).context("解析更新信息失败")
}

fn parse_version(raw: &str) -> anyhow::Result<Version> {
    let value = raw.trim().trim_start_matches('v');
    Version::parse(value).with_context(|| format!("版本号格式无效：{raw}"))
}

fn current_platform_key() -> Option<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        return None;
    };

    let arch = if cfg!(target_arch = "x86") {
        "i686"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "arm") {
        "armv7"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        return None;
    };

    Some(format!("{os}-{arch}"))
}
