//! 天工辅助构建任务。

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const WASM_TARGET: &str = "wasm32-wasip2";
const RUNTIME_MANIFEST: &str = "crates/tiangong-plugin-runtime/Cargo.toml";
const PLUGIN_DIST: &str = "target/plugin-dist";
const DEFAULT_OSS_BASE_URL: &str = "https://silent-tiangong.oss-cn-hangzhou.aliyuncs.com";
const PLUGIN_CATALOG_VERSION: u32 = 1;
const PRESERVED_DIRS: [&str; 3] = ["runtime", "logs", "data"];

#[derive(Debug, Deserialize)]
struct PublishedPluginCatalog {
    version: u32,
    plugins: Vec<PublishedPluginRelease>,
}

#[derive(Debug, Deserialize)]
struct PublishedPluginRelease {
    id: String,
    name: String,
    version: String,
    #[serde(default, rename = "description")]
    _description: String,
    manifest: PublishedRemoteArtifact,
    wasm: PublishedRemoteArtifact,
    #[serde(default)]
    signed_releases: BTreeMap<String, PublishedSignedRelease>,
    #[serde(default)]
    sidecars: BTreeMap<String, PublishedRemoteArtifact>,
    #[serde(default)]
    ui: BTreeMap<String, PublishedRemoteArtifact>,
}

#[derive(Debug, Deserialize)]
struct PublishedSignedRelease {
    url: String,
    signature_url: String,
}

#[derive(Debug, Deserialize)]
struct PublishedRemoteArtifact {
    url: String,
    checksum: String,
}

/// 单个 WASM 插件的构建配置。
struct PluginConfig {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    protocol_crate: &'static str,
    wasm_crate: &'static str,
    wasm_artifact: &'static str,
    sidecar_crate: Option<&'static str>,
    sidecar_artifact: Option<&'static str>,
    plugin_root: &'static str,
    plugin_manifest: &'static str,
    protocol_manifest: &'static str,
}

const MEMORY: PluginConfig = PluginConfig {
    id: "memory",
    name: "Memory",
    description: "对话记忆、召回与数据管理",
    protocol_crate: "tiangong-plugin-memory-protocol",
    wasm_crate: "tiangong-plugin-memory-wasm",
    wasm_artifact: "tiangong_plugin_memory_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-memory-sidecar"),
    sidecar_artifact: Some("tiangong-memory-sidecar"),
    plugin_root: "plugins/tiangong-plugin-memory",
    plugin_manifest: "plugins/tiangong-plugin-memory/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-memory/protocol/Cargo.toml",
};

const MCP: PluginConfig = PluginConfig {
    id: "mcp",
    name: "MCP",
    description: "MCP server 管理与工具桥接",
    protocol_crate: "tiangong-plugin-mcp-protocol",
    wasm_crate: "tiangong-plugin-mcp-wasm",
    wasm_artifact: "tiangong_plugin_mcp_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-mcp-sidecar"),
    sidecar_artifact: Some("tiangong-mcp-sidecar"),
    plugin_root: "plugins/tiangong-plugin-mcp",
    plugin_manifest: "plugins/tiangong-plugin-mcp/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-mcp/protocol/Cargo.toml",
};

const FETCH: PluginConfig = PluginConfig {
    id: "fetch",
    name: "Fetch",
    description: "URL 获取（text 提取正文 / download 落盘），含 SSRF 防护",
    protocol_crate: "tiangong-plugin-fetch-protocol",
    wasm_crate: "tiangong-plugin-fetch-wasm",
    wasm_artifact: "tiangong_plugin_fetch_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-fetch-sidecar"),
    sidecar_artifact: Some("tiangong-fetch-sidecar"),
    plugin_root: "plugins/tiangong-plugin-fetch",
    plugin_manifest: "plugins/tiangong-plugin-fetch/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-fetch/protocol/Cargo.toml",
};

const INDEX: PluginConfig = PluginConfig {
    id: "index",
    name: "Index",
    description: "工作区文件索引、对话历史索引与代码检索",
    protocol_crate: "tiangong-plugin-index-protocol",
    wasm_crate: "tiangong-plugin-index-wasm",
    wasm_artifact: "tiangong_plugin_index_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-index-sidecar"),
    sidecar_artifact: Some("tiangong-index-sidecar"),
    plugin_root: "plugins/tiangong-plugin-index",
    plugin_manifest: "plugins/tiangong-plugin-index/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-index/protocol/Cargo.toml",
};

const SCHEDULER: PluginConfig = PluginConfig {
    id: "scheduler",
    name: "Scheduler",
    description: "定时任务调度与执行",
    protocol_crate: "tiangong-plugin-scheduler-protocol",
    wasm_crate: "tiangong-plugin-scheduler-wasm",
    wasm_artifact: "tiangong_plugin_scheduler_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-scheduler-sidecar"),
    sidecar_artifact: Some("tiangong-scheduler-sidecar"),
    plugin_root: "plugins/tiangong-plugin-scheduler",
    plugin_manifest: "plugins/tiangong-plugin-scheduler/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-scheduler/protocol/Cargo.toml",
};

const SKILL: PluginConfig = PluginConfig {
    id: "skill",
    name: "Skill",
    description: "Skill 注册表管理、详情查询与 prompt 段落注入",
    protocol_crate: "tiangong-plugin-skill-protocol",
    wasm_crate: "tiangong-plugin-skill-wasm",
    wasm_artifact: "tiangong_plugin_skill_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-skill-sidecar"),
    sidecar_artifact: Some("tiangong-skill-sidecar"),
    plugin_root: "plugins/tiangong-plugin-skill",
    plugin_manifest: "plugins/tiangong-plugin-skill/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-skill/protocol/Cargo.toml",
};

const CODING: PluginConfig = PluginConfig {
    id: "coding",
    name: "Coding",
    description: "通用开发工作流、项目上下文与交付审查",
    protocol_crate: "tiangong-plugin-coding-protocol",
    wasm_crate: "tiangong-plugin-coding-wasm",
    wasm_artifact: "tiangong_plugin_coding_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-coding-sidecar"),
    sidecar_artifact: Some("tiangong-coding-sidecar"),
    plugin_root: "plugins/tiangong-plugin-coding",
    plugin_manifest: "plugins/tiangong-plugin-coding/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-coding/protocol/Cargo.toml",
};

const PROMPT: PluginConfig = PluginConfig {
    id: "prompt",
    name: "Prompt",
    description: "产品文案与自定义指令注入",
    protocol_crate: "tiangong-plugin-prompt-protocol",
    wasm_crate: "tiangong-plugin-prompt-wasm",
    wasm_artifact: "tiangong_plugin_prompt_wasm.wasm",
    sidecar_crate: None,
    sidecar_artifact: None,
    plugin_root: "plugins/tiangong-plugin-prompt",
    plugin_manifest: "plugins/tiangong-plugin-prompt/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-prompt/protocol/Cargo.toml",
};

const FS: PluginConfig = PluginConfig {
    id: "fs",
    name: "Fs",
    description: "基础文件工具（读写/补丁/目录树）+ 进程级文件锁表",
    protocol_crate: "tiangong-plugin-fs-protocol",
    wasm_crate: "tiangong-plugin-fs-wasm",
    wasm_artifact: "tiangong_plugin_fs_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-fs-sidecar"),
    sidecar_artifact: Some("tiangong-fs-sidecar"),
    plugin_root: "plugins/tiangong-plugin-fs",
    plugin_manifest: "plugins/tiangong-plugin-fs/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-fs/protocol/Cargo.toml",
};

const COMMAND: PluginConfig = PluginConfig {
    id: "command",
    name: "Command",
    description: "基础命令执行（run_command/run_shell）+ 命令校验策略",
    protocol_crate: "tiangong-plugin-command-protocol",
    wasm_crate: "tiangong-plugin-command-wasm",
    wasm_artifact: "tiangong_plugin_command_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-command-sidecar"),
    sidecar_artifact: Some("tiangong-command-sidecar"),
    plugin_root: "plugins/tiangong-plugin-command",
    plugin_manifest: "plugins/tiangong-plugin-command/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-command/protocol/Cargo.toml",
};

const COMPUTER_USE: PluginConfig = PluginConfig {
    id: "computer-use",
    name: "Computer Use",
    description: "跨平台桌面应用控制（Windows UI Automation / macOS AXUIElement / Linux AT-SPI2）",
    protocol_crate: "tiangong-plugin-computer-use-protocol",
    wasm_crate: "tiangong-plugin-computer-use-wasm",
    wasm_artifact: "tiangong_plugin_computer_use_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-computer-use-sidecar"),
    sidecar_artifact: Some("tiangong-computer-use-sidecar"),
    plugin_root: "plugins/tiangong-plugin-computer-use",
    plugin_manifest: "plugins/tiangong-plugin-computer-use/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-computer-use/protocol/Cargo.toml",
};

const TEXT_TO_SPEECH: PluginConfig = PluginConfig {
    id: "text-to-speech",
    name: "Text To Speech",
    description: "Text to Speech",
    protocol_crate: "tiangong-plugin-text-to-speech-protocol",
    wasm_crate: "tiangong-plugin-text-to-speech-wasm",
    wasm_artifact: "tiangong_plugin_text_to_speech_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-text-to-speech-sidecar"),
    sidecar_artifact: Some("tiangong-text-to-speech-sidecar"),
    plugin_root: "plugins/tiangong-plugin-text-to-speech",
    plugin_manifest: "plugins/tiangong-plugin-text-to-speech/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-text-to-speech/protocol/Cargo.toml",
};

const GENERATE_IMAGE: PluginConfig = PluginConfig {
    id: "generate-image",
    name: "Generate Image",
    description: "Generate Image",
    protocol_crate: "tiangong-plugin-generate-image-protocol",
    wasm_crate: "tiangong-plugin-generate-image-wasm",
    wasm_artifact: "tiangong_plugin_generate_image_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-generate-image-sidecar"),
    sidecar_artifact: Some("tiangong-generate-image-sidecar"),
    plugin_root: "plugins/tiangong-plugin-generate-image",
    plugin_manifest: "plugins/tiangong-plugin-generate-image/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-generate-image/protocol/Cargo.toml",
};

const GENERATE_IMAGE_OPENAI: PluginConfig = PluginConfig {
    id: "generate-image-openai",
    name: "Generate Image OpenAI",
    description: "Generate Image OpenAI",
    protocol_crate: "tiangong-plugin-generate-image-openai-protocol",
    wasm_crate: "tiangong-plugin-generate-image-openai-wasm",
    wasm_artifact: "tiangong_plugin_generate_image_openai_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-generate-image-openai-sidecar"),
    sidecar_artifact: Some("tiangong-generate-image-openai-sidecar"),
    plugin_root: "plugins/tiangong-plugin-generate-image-openai",
    plugin_manifest: "plugins/tiangong-plugin-generate-image-openai/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-generate-image-openai/protocol/Cargo.toml",
};

const ANALYZE_ATTACHMENT: PluginConfig = PluginConfig {
    id: "analyze-attachment",
    name: "Analyze Attachment",
    description: "Analyze Attachment",
    protocol_crate: "tiangong-plugin-analyze-attachment-protocol",
    wasm_crate: "tiangong-plugin-analyze-attachment-wasm",
    wasm_artifact: "tiangong_plugin_analyze_attachment_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-analyze-attachment-sidecar"),
    sidecar_artifact: Some("tiangong-analyze-attachment-sidecar"),
    plugin_root: "plugins/tiangong-plugin-analyze-attachment",
    plugin_manifest: "plugins/tiangong-plugin-analyze-attachment/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-analyze-attachment/protocol/Cargo.toml",
};

const SPEECH_TO_TEXT: PluginConfig = PluginConfig {
    id: "speech-to-text",
    name: "Speech To Text",
    description: "Speech To Text",
    protocol_crate: "tiangong-plugin-speech-to-text-protocol",
    wasm_crate: "tiangong-plugin-speech-to-text-wasm",
    wasm_artifact: "tiangong_plugin_speech_to_text_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-speech-to-text-sidecar"),
    sidecar_artifact: Some("tiangong-speech-to-text-sidecar"),
    plugin_root: "plugins/tiangong-plugin-speech-to-text",
    plugin_manifest: "plugins/tiangong-plugin-speech-to-text/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-speech-to-text/protocol/Cargo.toml",
};

const GENERATE_VIDEO: PluginConfig = PluginConfig {
    id: "generate-video",
    name: "Generate Video",
    description: "Generate Video",
    protocol_crate: "tiangong-plugin-generate-video-protocol",
    wasm_crate: "tiangong-plugin-generate-video-wasm",
    wasm_artifact: "tiangong_plugin_generate_video_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-generate-video-sidecar"),
    sidecar_artifact: Some("tiangong-generate-video-sidecar"),
    plugin_root: "plugins/tiangong-plugin-generate-video",
    plugin_manifest: "plugins/tiangong-plugin-generate-video/plugin.json",
    protocol_manifest: "plugins/tiangong-plugin-generate-video/protocol/Cargo.toml",
};

const SCREENSHOT_INPUT: PluginConfig = PluginConfig {
    id: "screenshot-input",
    name: "Screenshot Input",
    description: "跨平台区域截图并加入当前输入草稿",
    protocol_crate: "tiangong-plugin-screenshot-input-protocol",
    wasm_crate: "tiangong-plugin-screenshot-input-wasm",
    wasm_artifact: "tiangong_plugin_screenshot_input_wasm.wasm",
    sidecar_crate: Some("tiangong-plugin-screenshot-input-sidecar"),
    sidecar_artifact: Some("tiangong-screenshot-input-sidecar"),
    plugin_root: "plugins/screenshot-input",
    plugin_manifest: "plugins/screenshot-input/plugin.json",
    protocol_manifest: "plugins/screenshot-input/protocol/Cargo.toml",
};

fn plugin_ui_entries(config: &PluginConfig) -> &'static [&'static str] {
    match config.id {
        "screenshot-input" => &["dist/index.html"],
        _ => &[],
    }
}

fn plugin_config(id: &str) -> io::Result<&'static PluginConfig> {
    match id {
        "memory" => Ok(&MEMORY),
        "mcp" => Ok(&MCP),
        "fetch" => Ok(&FETCH),
        "index" => Ok(&INDEX),
        "scheduler" => Ok(&SCHEDULER),
        "skill" => Ok(&SKILL),
        "coding" => Ok(&CODING),
        "prompt" => Ok(&PROMPT),
        "text-to-speech" => Ok(&TEXT_TO_SPEECH),
        "generate-image" => Ok(&GENERATE_IMAGE),
        "generate-image-openai" => Ok(&GENERATE_IMAGE_OPENAI),
        "analyze-attachment" => Ok(&ANALYZE_ATTACHMENT),
        "speech-to-text" => Ok(&SPEECH_TO_TEXT),
        "generate-video" => Ok(&GENERATE_VIDEO),
        "fs" => Ok(&FS),
        "command" => Ok(&COMMAND),
        "computer-use" => Ok(&COMPUTER_USE),
        "screenshot-input" => Ok(&SCREENSHOT_INPUT),
        other => Err(invalid_input(format!("暂不支持插件: {other}"))),
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = match args.as_slice() {
        [command, plugin] if command == "build-plugin" => match plugin_config(plugin) {
            Ok(config) => build_plugin(config),
            Err(error) => Err(error),
        },
        [command, plugin, output] if command == "build-plugin-wasm" => {
            match plugin_config(plugin) {
                Ok(config) => build_plugin_wasm(config, output),
                Err(error) => Err(error),
            }
        }
        [command, plugin] if command == "validate-plugin" => match plugin_config(plugin) {
            Ok(config) => validate_plugin(config),
            Err(error) => Err(error),
        },
        [command, input, output] if command == "merge-plugin-dist" => {
            merge_plugin_distributions(Path::new(input), Path::new(output), None)
        }
        [command, plugin, input, output] if command == "merge-plugin-dist" => plugin_config(plugin)
            .and_then(|_| {
                merge_plugin_distributions(Path::new(input), Path::new(output), Some(plugin))
            }),
        [command, current, plugin_release, output] if command == "merge-plugin-catalog" => {
            merge_plugin_catalog(
                Path::new(current),
                Path::new(plugin_release),
                Path::new(output),
            )
        }
        [command, catalog] if command == "validate-plugin-catalog" => {
            validate_plugin_catalog_file(Path::new(catalog))
        }
        [command, plugin_id] if command == "new-plugin" => new_plugin(plugin_id, None),
        [command, plugin_id, output_dir] if command == "new-plugin" => {
            new_plugin(plugin_id, Some(output_dir))
        }
        [command] if command == "build-wasm" || command == "build-sidecar" => {
            eprintln!("[xtask] {command} 已合并到 build-plugin <id>");
            Err(invalid_input("请使用 build-plugin <id>"))
        }
        [command] if command == "help" || command == "--help" || command == "-h" => {
            print_help();
            Ok(())
        }
        [] => {
            print_help();
            Ok(())
        }
        _ => {
            print_help();
            Err(invalid_input("参数无效"))
        }
    };

    if let Err(error) = result {
        eprintln!("xtask 失败: {error}");
        std::process::exit(1);
    }
}

fn print_help() {
    eprintln!("xtask - 天工辅助构建任务\n");
    eprintln!("用法:");
    eprintln!("  cargo run -p xtask -- validate-plugin <id>");
    eprintln!("  cargo run -p xtask -- build-plugin-wasm <id> <输出WASM>");
    eprintln!("  cargo run -p xtask -- build-plugin <id>");
    eprintln!("  cargo run -p xtask -- merge-plugin-dist [plugin-id] <输入目录> <输出目录>");
    eprintln!(
        "  cargo run -p xtask -- merge-plugin-catalog <当前catalog或-> <插件release> <输出catalog>"
    );
    eprintln!("  cargo run -p xtask -- validate-plugin-catalog <catalog或->");
}

fn validate_plugin(config: &PluginConfig) -> io::Result<()> {
    let workspace_root = workspace_root();
    validate_versions(&workspace_root, config)?;
    eprintln!("[xtask] validation passed");
    Ok(())
}

fn build_plugin_wasm(config: &PluginConfig, output: &str) -> io::Result<()> {
    let workspace_root = workspace_root();
    validate_versions(&workspace_root, config)?;
    eprintln!("[xtask] CI build-plugin-wasm");
    run_cargo(&workspace_root, &["check", "-p", config.protocol_crate])?;
    run_cargo(
        &workspace_root,
        &[
            "check",
            "-p",
            config.protocol_crate,
            "--target",
            WASM_TARGET,
        ],
    )?;
    run_cargo(
        &workspace_root,
        &[
            "build",
            "-p",
            config.wasm_crate,
            "--target",
            WASM_TARGET,
            "--release",
        ],
    )?;
    eprintln!("[xtask] WASM check ok");
    let wasm = workspace_root
        .join("target")
        .join(WASM_TARGET)
        .join("release")
        .join(config.wasm_artifact);
    require_file(&wasm)?;
    let dest = Path::new(output);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&wasm, dest)?;
    eprintln!("[xtask] WASM copied to: {}", dest.display());
    Ok(())
}

fn build_plugin(config: &PluginConfig) -> io::Result<()> {
    let workspace_root = workspace_root();
    validate_versions(&workspace_root, config)?;

    let plugin_name = config.name;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（native）...");
    run_cargo(&workspace_root, &["check", "-p", config.protocol_crate])?;
    eprintln!("[xtask] 检查 {plugin_name} 私有协议（{WASM_TARGET}）...");
    run_cargo(
        &workspace_root,
        &[
            "check",
            "-p",
            config.protocol_crate,
            "--target",
            WASM_TARGET,
        ],
    )?;
    let wasm = match std::env::var("TIANGONG_PLUGIN_PREBUILT_WASM") {
        Ok(path) => {
            eprintln!("[xtask] using prebuilt wasm");
            let p = PathBuf::from(path);
            require_file(&p)?;
            p
        }
        Err(_) => {
            eprintln!("[xtask] build wasm...");
            run_cargo(
                &workspace_root,
                &[
                    "build",
                    "-p",
                    config.wasm_crate,
                    "--target",
                    WASM_TARGET,
                    "--release",
                ],
            )?;
            let p = workspace_root
                .join("target")
                .join(WASM_TARGET)
                .join("release")
                .join(config.wasm_artifact);
            require_file(&p)?;
            p
        }
    };

    let plugins_dir = storage_root().join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let staging = plugins_dir.join(format!(".{}-staging-{}", config.id, std::process::id()));
    let destination = plugins_dir.join(config.id);
    remove_dir_if_exists(&staging)?;
    std::fs::create_dir_all(&staging)?;

    let staged_wasm = staging.join(config.wasm_artifact);
    std::fs::copy(&wasm, &staged_wasm)?;

    // sidecar 构建和复制（无 sidecar 的插件跳过）。
    if let (Some(sidecar_crate), Some(sidecar_artifact)) =
        (config.sidecar_crate, config.sidecar_artifact)
    {
        eprintln!("[xtask] 构建 {plugin_name} sidecar...");
        run_cargo(
            &workspace_root,
            &["build", "-p", sidecar_crate, "--release"],
        )?;
        let sidecar = workspace_root.join("target").join("release").join(format!(
            "{sidecar_artifact}{}",
            std::env::consts::EXE_SUFFIX
        ));
        require_file(&sidecar)?;
        let staged_sidecar = staging.join(format!(
            "{sidecar_artifact}{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::copy(&sidecar, &staged_sidecar)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged_sidecar, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    std::fs::copy(
        workspace_root.join(config.plugin_manifest),
        staging.join("plugin.json"),
    )?;
    stage_plugin_ui(&workspace_root, &staging, config)?;
    for directory in PRESERVED_DIRS {
        std::fs::create_dir_all(staging.join(directory))?;
    }

    eprintln!("[xtask] WASM sha256: {}", sha256(&staged_wasm)?);
    if let Some(sidecar_artifact) = config.sidecar_artifact {
        let staged_sidecar = staging.join(format!(
            "{sidecar_artifact}{}",
            std::env::consts::EXE_SUFFIX
        ));
        eprintln!("[xtask] sidecar sha256: {}", sha256(&staged_sidecar)?);
    }
    generate_oss_distribution(&workspace_root, &staging, config)?;
    deploy_atomically(&staging, &destination, config)?;
    eprintln!(
        "[xtask] {plugin_name} 插件已部署到: {}",
        destination.display()
    );
    Ok(())
}

fn stage_plugin_ui(workspace_root: &Path, staging: &Path, config: &PluginConfig) -> io::Result<()> {
    let entries = plugin_ui_entries(config);
    if entries.is_empty() {
        return Ok(());
    }

    let prebuilt = std::env::var_os("TIANGONG_PLUGIN_PREBUILT_UI").map(PathBuf::from);
    if prebuilt.is_none() {
        let plugin_root = workspace_root.join(config.plugin_root);
        eprintln!("[xtask] 安装并构建 {} UI...", config.name);
        run_yarn(&plugin_root, &["install", "--frozen-lockfile"])?;
        run_yarn(&plugin_root, &["build"])?;
    } else if entries.len() != 1 {
        return Err(invalid_input(
            "TIANGONG_PLUGIN_PREBUILT_UI 仅支持单入口 UI 插件",
        ));
    }

    for entry in entries {
        let source = prebuilt
            .as_ref()
            .cloned()
            .unwrap_or_else(|| workspace_root.join(config.plugin_root).join(entry));
        require_file(&source)?;
        let destination = staging.join(entry);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, &destination)?;
    }
    Ok(())
}

/// 生成插件签名发布清单（release.json）并对它做 minisign 数字签名。
///
/// 清单结构与运行时 `signature.rs::SignedPluginRelease` 对齐，覆盖
/// schema、id、version、publisher、permissions 以及 plugin.json、WASM
/// UI 入口和（若有）sidecar 的 sha256。签名失败直接报错，绝不写空签名文件。
fn write_signed_release(plugin: &Path, config: &PluginConfig) -> io::Result<()> {
    let manifest_path = plugin.join("plugin.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 plugin.json 失败: {error}")))?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    let permissions = manifest
        .get("permissions")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    let mut release = serde_json::json!({
        "schema_version": 1,
        "id": config.id,
        "version": version,
        "publisher": "tiangong-official",
        "permissions": permissions,
        "manifest": {
            "path": "plugin.json",
            "sha256": sha256(&manifest_path)?,
        },
        "wasm": {
            "path": config.wasm_artifact,
            "sha256": sha256(&plugin.join(config.wasm_artifact))?,
        },
    });

    // sidecar 声明（无 sidecar 的插件跳过，保持清单与 plugin.json 一致）。
    if let Some(sidecar_artifact) = config.sidecar_artifact {
        let sidecar_name = format!("{sidecar_artifact}{}", std::env::consts::EXE_SUFFIX);
        release["sidecar"] = serde_json::json!({
            "path": sidecar_name,
            "sha256": sha256(&plugin.join(&sidecar_name))?,
        });
    }
    let ui = plugin_ui_entries(config)
        .iter()
        .map(|entry| {
            Ok(serde_json::json!({
                "path": entry,
                "sha256": sha256(&plugin.join(entry))?,
            }))
        })
        .collect::<io::Result<Vec<_>>>()?;
    if !ui.is_empty() {
        release["ui"] = serde_json::Value::Array(ui);
    }

    let release_path = plugin.join("release.json");
    write_json(&release_path, &release)?;

    let key_path = std::env::var_os("TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            invalid_input("缺少插件签名私钥，请设置 TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH")
        })?;
    let password =
        std::env::var("TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PASSWORD").unwrap_or_default();
    let status = Command::new("cargo")
        .args(["tauri", "signer", "sign", "-f"])
        .arg(&key_path)
        .args(["-p", &password])
        .arg(&release_path)
        .status()?;
    if !status.success() {
        return Err(invalid_data("插件发布清单签名失败"));
    }
    eprintln!("[xtask] release.json 已签名");
    Ok(())
}

fn generate_oss_distribution(
    workspace_root: &Path,
    plugin: &Path,
    config: &PluginConfig,
) -> io::Result<()> {
    // 只有带 sidecar 的插件才生成签名发布清单：签名用于建立 sidecar 信任边界，
    // 纯 WASM 插件（如 prompt）不需要 sidecar，运行时不强制要求签名。
    let has_sidecar = config.sidecar_artifact.is_some();
    if has_sidecar {
        write_signed_release(plugin, config)?;
    }

    let manifest_path = plugin.join("plugin.json");
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", manifest_path.display())))?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    let base_url = env_var_or("TIANGONG_PLUGIN_OSS_BASE_URL", DEFAULT_OSS_BASE_URL)
        .trim_end_matches('/')
        .to_string();
    let release_url = format!("{base_url}/plugins/{}/{}", config.id, version);
    let platform = current_platform_key();
    let dist_root = workspace_root.join(PLUGIN_DIST);
    let release_root = dist_root.join("plugins").join(config.id).join(version);
    let platform_root = release_root.join(&platform);
    let index_root = dist_root.join("plugins-index");
    std::fs::create_dir_all(&platform_root)?;
    std::fs::create_dir_all(index_root.join("fragments"))?;

    let dist_manifest = release_root.join("plugin.json");
    let dist_wasm = release_root.join(config.wasm_artifact);
    std::fs::copy(&manifest_path, &dist_manifest)?;
    std::fs::copy(plugin.join(config.wasm_artifact), &dist_wasm)?;
    let mut ui_artifacts = serde_json::Map::new();
    for entry in plugin_ui_entries(config) {
        let destination = release_root.join(entry);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(plugin.join(entry), &destination)?;
        ui_artifacts.insert(
            (*entry).to_string(),
            serde_json::json!({
                "url": format!("{release_url}/{entry}"),
                "checksum": format!("sha256:{}", sha256(&destination)?),
            }),
        );
    }
    // 签名清单与签名文件复制到分平台目录，供运行时验签（仅 sidecar 插件）。
    if has_sidecar {
        std::fs::copy(
            plugin.join("release.json"),
            platform_root.join("release.json"),
        )?;
        std::fs::copy(
            plugin.join("release.json.sig"),
            platform_root.join("release.json.sig"),
        )?;
    }

    // sidecar 分发（无 sidecar 的插件跳过）。
    let sidecar_entry = if let Some(sidecar_artifact) = config.sidecar_artifact {
        let dist_sidecar = platform_root.join(format!(
            "{sidecar_artifact}{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::copy(
            plugin.join(format!(
                "{sidecar_artifact}{}",
                std::env::consts::EXE_SUFFIX
            )),
            &dist_sidecar,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dist_sidecar, std::fs::Permissions::from_mode(0o755))?;
        }
        let sidecar_checksum = format!("sha256:{}", sha256(&dist_sidecar)?);
        Some(serde_json::json!({
            platform.clone(): {
                "url": format!(
                    "{release_url}/{platform}/{}{}",
                    sidecar_artifact,
                    std::env::consts::EXE_SUFFIX
                ),
                "checksum": sidecar_checksum,
            }
        }))
    } else {
        None
    };

    let manifest_checksum = format!("sha256:{}", sha256(&dist_manifest)?);
    let wasm_checksum = format!("sha256:{}", sha256(&dist_wasm)?);
    let mut release = serde_json::json!({
        "id": config.id,
        "name": config.name,
        "version": version,
        "description": config.description,
        "manifest": {
            "url": format!("{release_url}/plugin.json"),
            "checksum": manifest_checksum,
        },
        "wasm": {
            "url": format!("{release_url}/{}", config.wasm_artifact),
            "checksum": wasm_checksum,
        },
        "sidecars": sidecar_entry.unwrap_or_else(|| serde_json::json!({})),
        "ui": ui_artifacts,
    });
    // 签名清单仅对 sidecar 插件生成，纯 WASM 插件不携带签名。
    if has_sidecar {
        release["signed_releases"] = serde_json::json!({
            platform.clone(): {
                "url": format!("{release_url}/{platform}/release.json"),
                "signature_url": format!("{release_url}/{platform}/release.json.sig"),
            }
        });
    }
    write_json(
        &index_root.join("catalog.json"),
        &serde_json::json!({"version": 1, "plugins": [release.clone()]}),
    )?;
    write_json(
        &index_root
            .join("fragments")
            .join(format!("{}-{platform}.json", config.id)),
        &release,
    )?;

    let mut checksums = format!(
        "{}  plugin.json\n{}  {}\n",
        sha256(&dist_manifest)?,
        sha256(&dist_wasm)?,
        config.wasm_artifact,
    );
    if let Some(sidecar_artifact) = config.sidecar_artifact {
        let dist_sidecar = platform_root.join(format!(
            "{sidecar_artifact}{}",
            std::env::consts::EXE_SUFFIX
        ));
        checksums.push_str(&format!(
            "{}  {}/{}{}\n",
            sha256(&dist_sidecar)?,
            platform,
            sidecar_artifact,
            std::env::consts::EXE_SUFFIX,
        ));
    }
    for entry in plugin_ui_entries(config) {
        checksums.push_str(&format!(
            "{}  {}\n",
            sha256(&release_root.join(entry))?,
            entry,
        ));
    }
    std::fs::write(
        release_root.join(format!("SHA256SUMS-{platform}")),
        checksums,
    )?;

    eprintln!("[xtask] OSS 制品已生成: {}", dist_root.display());
    Ok(())
}

fn write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut content = serde_json::to_vec_pretty(value)
        .map_err(|error| invalid_data(format!("生成 {} 失败: {error}", path.display())))?;
    content.push(b'\n');
    std::fs::write(path, content)
}

fn validate_plugin_catalog_file(path: &Path) -> io::Result<()> {
    let mut content = Vec::new();
    if path == Path::new("-") {
        io::stdin().read_to_end(&mut content)?;
    } else {
        content = std::fs::read(path)?;
    }
    let value: serde_json::Value = serde_json::from_slice(&content)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", path.display())))?;
    let plugin_count = validate_plugin_catalog_value(&value, path)?;
    eprintln!(
        "[xtask] 插件目录验证通过: {}（{plugin_count} 个插件）",
        path.display()
    );
    Ok(())
}

fn validate_plugin_catalog_value(value: &serde_json::Value, path: &Path) -> io::Result<usize> {
    let catalog: PublishedPluginCatalog = serde_json::from_value(value.clone())
        .map_err(|error| invalid_data(format!("插件目录结构无效: {}: {error}", path.display())))?;
    if catalog.version != PLUGIN_CATALOG_VERSION {
        return Err(invalid_data(format!(
            "插件目录版本无效: expected={PLUGIN_CATALOG_VERSION}, actual={}",
            catalog.version
        )));
    }

    let mut ids = BTreeSet::new();
    for plugin in &catalog.plugins {
        if plugin.id.trim().is_empty() || !ids.insert(plugin.id.as_str()) {
            return Err(invalid_data(format!(
                "插件目录包含空 ID 或重复 ID: {}",
                plugin.id
            )));
        }
        if plugin.name.trim().is_empty() {
            return Err(invalid_data(format!("插件 {} 的名称为空", plugin.id)));
        }
        semver::Version::parse(&plugin.version).map_err(|error| {
            invalid_data(format!(
                "插件 {} 版本 {} 无效: {error}",
                plugin.id, plugin.version
            ))
        })?;
        validate_catalog_artifact(&plugin.manifest, "插件清单")?;
        validate_catalog_artifact(&plugin.wasm, "WASM 制品")?;
        for (entry, artifact) in &plugin.ui {
            validate_ui_entry(entry)?;
            validate_catalog_artifact(artifact, "UI 制品")?;
        }

        for (platform, artifact) in &plugin.sidecars {
            if platform.trim().is_empty() {
                return Err(invalid_data(format!(
                    "插件 {} 包含空 sidecar 平台",
                    plugin.id
                )));
            }
            validate_catalog_artifact(artifact, "sidecar 制品")?;
            let signed = plugin.signed_releases.get(platform).ok_or_else(|| {
                invalid_data(format!("插件 {} 的平台 {platform} 缺少签名清单", plugin.id))
            })?;
            validate_catalog_url(&signed.url, "插件签名清单")?;
            validate_catalog_url(&signed.signature_url, "插件签名文件")?;
        }
        for signed in plugin.signed_releases.values() {
            validate_catalog_url(&signed.url, "插件签名清单")?;
            validate_catalog_url(&signed.signature_url, "插件签名文件")?;
        }
    }
    Ok(catalog.plugins.len())
}

fn validate_catalog_artifact(artifact: &PublishedRemoteArtifact, label: &str) -> io::Result<()> {
    validate_catalog_url(&artifact.url, label)?;
    let checksum = artifact
        .checksum
        .strip_prefix("sha256:")
        .ok_or_else(|| invalid_data(format!("{label} checksum 必须使用 sha256:<hex> 格式")))?;
    let bytes = hex::decode(checksum)
        .map_err(|error| invalid_data(format!("{label} checksum 不是有效十六进制: {error}")))?;
    if bytes.len() != 32 {
        return Err(invalid_data(format!("{label} SHA-256 长度无效")));
    }
    Ok(())
}

fn validate_catalog_url(value: &str, label: &str) -> io::Result<()> {
    let parsed = url::Url::parse(value)
        .map_err(|error| invalid_data(format!("{label} URL 无效: {value}: {error}")))?;
    let local_http = parsed.scheme() == "http"
        && parsed
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
    if parsed.scheme() != "https" && !local_http {
        return Err(invalid_data(format!("{label} 必须使用 HTTPS: {value}")));
    }
    Ok(())
}

fn validate_ui_entry(entry: &str) -> io::Result<()> {
    let path = Path::new(entry);
    if entry.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid_data(format!("UI 制品路径无效: {entry}")));
    }
    Ok(())
}

fn current_platform_key() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };
    format!("{os}-{arch}")
}

fn validate_versions(workspace_root: &Path, config: &PluginConfig) -> io::Result<()> {
    let protocol = read_toml(&workspace_root.join(config.protocol_manifest))?;
    let crate_version = protocol
        .get("package")
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid_data("无法读取 protocol package.version"))?;

    let manifest_path = workspace_root.join(config.plugin_manifest);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", manifest_path.display())))?;
    let manifest_version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid_data("plugin.json 缺少 version"))?;
    if manifest_version != crate_version {
        return Err(invalid_data(format!(
            "插件版本不一致: crate={crate_version}, plugin.json={manifest_version}"
        )));
    }

    // business-protocol / transport-protocol 校验（仅 sidecar 插件需要）。
    if config.sidecar_artifact.is_some() {
        let protocol = read_toml(&workspace_root.join(config.protocol_manifest))?;
        let business_protocol = protocol
            .get("package")
            .and_then(|value| value.get("metadata"))
            .and_then(|value| value.get("tiangong"))
            .and_then(|value| value.get("business-protocol"))
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| invalid_data("Protocol 缺少 business-protocol 元数据"))?;
        let manifest_business_protocol = manifest
            .get("sidecar")
            .and_then(|value| value.get("business_protocol"))
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| invalid_data("plugin.json 缺少 sidecar.business_protocol"))?;
        if u64::try_from(business_protocol).ok() != Some(manifest_business_protocol) {
            return Err(invalid_data(format!(
                "业务协议版本不一致: protocol={business_protocol}, plugin.json={manifest_business_protocol}"
            )));
        }

        let runtime = read_toml(&workspace_root.join(RUNTIME_MANIFEST))?;
        let transport_protocol = runtime
            .get("package")
            .and_then(|value| value.get("metadata"))
            .and_then(|value| value.get("tiangong"))
            .and_then(|value| value.get("transport-protocol"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| invalid_data("插件运行时缺少 transport-protocol 元数据"))?;
        let manifest_transport_protocol = manifest
            .get("sidecar")
            .and_then(|value| value.get("transport_protocol"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid_data("plugin.json 缺少 sidecar.transport_protocol"))?;
        if transport_protocol != manifest_transport_protocol {
            return Err(invalid_data(format!(
                "sidecar transport 版本不一致: runtime={transport_protocol}, plugin.json={manifest_transport_protocol}"
            )));
        }
    }

    let wasm_binary = manifest
        .get("wasm")
        .and_then(|value| value.get("binary"))
        .and_then(serde_json::Value::as_str);
    let sidecar_binary = manifest
        .get("sidecar")
        .and_then(|value| value.get("binary"))
        .and_then(serde_json::Value::as_str);
    if wasm_binary != Some(config.wasm_artifact) {
        return Err(invalid_data("plugin.json wasm.binary 与构建产物不一致"));
    }
    if let Some(expected) = config.sidecar_artifact
        && sidecar_binary != Some(expected)
    {
        return Err(invalid_data("plugin.json sidecar.binary 与构建产物不一致"));
    }
    let manifest_ui_entries = manifest
        .get("ui")
        .and_then(|value| value.get("contributions"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.get("entry").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    let configured_ui_entries = plugin_ui_entries(config)
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if manifest_ui_entries != configured_ui_entries {
        return Err(invalid_data(format!(
            "plugin.json UI 入口与构建配置不一致: manifest={manifest_ui_entries:?}, config={configured_ui_entries:?}"
        )));
    }

    let plugin_root = workspace_root.join(config.plugin_root);
    require_file(&plugin_root.join("wasm/Cargo.toml"))?;
    require_file(&plugin_root.join("protocol/Cargo.toml"))?;
    if config.sidecar_artifact.is_some() {
        require_file(&plugin_root.join("sidecar/Cargo.toml"))?;
    }
    Ok(())
}

fn deploy_atomically(staging: &Path, destination: &Path, config: &PluginConfig) -> io::Result<()> {
    if !destination.exists() {
        return std::fs::rename(staging, destination);
    }

    let parent = destination
        .parent()
        .ok_or_else(|| invalid_input("插件安装目录缺少父目录"))?;
    let backup = parent.join(format!(".{}-backup-{}", config.id, std::process::id()));
    remove_dir_if_exists(&backup)?;
    std::fs::rename(destination, &backup)?;

    let result = (|| {
        for directory in PRESERVED_DIRS {
            let staged = staging.join(directory);
            let preserved = backup.join(directory);
            if preserved.exists() {
                remove_dir_if_exists(&staged)?;
                std::fs::rename(preserved, staged)?;
            }
        }
        std::fs::rename(staging, destination)
    })();

    if let Err(error) = result {
        for directory in PRESERVED_DIRS {
            let staged = staging.join(directory);
            let preserved = backup.join(directory);
            if staged.exists() && !preserved.exists() {
                let _ = std::fs::rename(staged, preserved);
            }
        }
        let _ = std::fs::rename(&backup, destination);
        return Err(error);
    }

    remove_dir_if_exists(&backup)
}

fn run_cargo(workspace_root: &Path, args: &[&str]) -> io::Result<()> {
    let status = Command::new(env_var_or("CARGO", "cargo"))
        .current_dir(workspace_root)
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cargo {} 执行失败",
            args.join(" ")
        )))
    }
}

fn run_yarn(directory: &Path, args: &[&str]) -> io::Result<()> {
    let status = Command::new(env_var_or("YARN", "yarn"))
        .current_dir(directory)
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "yarn {} 执行失败",
            args.join(" ")
        )))
    }
}

fn read_toml(path: &Path) -> io::Result<toml::Value> {
    let content = std::fs::read_to_string(path)?;
    toml::from_str(&content)
        .map_err(|error| invalid_data(format!("解析 {} 失败: {error}", path.display())))
}

fn require_file(path: &Path) -> io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("文件不存在: {}", path.display()),
        ))
    }
}

fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn sha256(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|value| !value.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|value| !value.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buffer = PathBuf::from(drive);
            buffer.push(path);
            Some(buffer)
        }
        _ => None,
    }
}

fn env_var_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 从模板生成纯 UI 插件骨架：复制 templates/ui-app 并替换插件 ID。
fn new_plugin(plugin_id: &str, output_dir: Option<&str>) -> io::Result<()> {
    if plugin_id.is_empty()
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_input(
            "插件 ID 只能包含字母数字与 - _ .（建议反向域名如 com.example.board）",
        ));
    }
    let manifest_dir = Path::new("plugins").join("templates").join("ui-app");
    if !manifest_dir.join("plugin.json").is_file() {
        return Err(invalid_input(format!(
            "模板目录不存在: {}（请在仓库根目录执行）",
            manifest_dir.display()
        )));
    }
    let target = match output_dir {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from("dist").join("plugins").join(plugin_id),
    };
    if target.exists() {
        return Err(invalid_input(format!(
            "目标目录已存在: {}",
            target.display()
        )));
    }
    copy_dir_recursive(&manifest_dir, &target)?;
    let manifest_path = target.join("plugin.json");
    let manifest = std::fs::read_to_string(&manifest_path)?.replace("com.example.board", plugin_id);
    std::fs::write(&manifest_path, manifest)?;
    println!("[xtask] 插件骨架已生成: {}", target.display());
    println!("[xtask] 本地导入: 在天工「设置 → 插件管理 → 导入本地插件」选择该目录");
    println!("[xtask] 工程化开发: 参阅 plugins/sdk/README.md 与 docs/plugin-development.md");
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn merge_plugin_distributions(
    input_root: &Path,
    output_root: &Path,
    expected_plugin: Option<&str>,
) -> io::Result<()> {
    if !input_root.is_dir() {
        return Err(invalid_input(format!(
            "插件平台制品输入目录不存在: {}",
            input_root.display()
        )));
    }
    remove_dir_if_exists(output_root)?;
    std::fs::create_dir_all(output_root.join("plugins-index"))?;

    let mut fragments = Vec::new();
    collect_fragment_files(input_root, &mut fragments)?;
    if fragments.is_empty() {
        return Err(invalid_data("没有找到插件目录片段"));
    }
    fragments.sort();

    let mut releases = BTreeMap::<String, serde_json::Value>::new();
    let mut fragment_platforms = BTreeMap::<String, BTreeSet<String>>::new();
    for fragment_path in fragments {
        let release: serde_json::Value = serde_json::from_slice(&std::fs::read(&fragment_path)?)
            .map_err(|error| {
                invalid_data(format!("解析 {} 失败: {error}", fragment_path.display()))
            })?;
        let id = required_json_string(&release, "id", &fragment_path)?;
        if expected_plugin.is_some_and(|expected| expected != id) {
            continue;
        }
        let version = required_json_string(&release, "version", &fragment_path)?;
        let file_stem = fragment_path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_data(format!("{} 文件名无效", fragment_path.display())))?;
        let prefix = format!("{id}-");
        let platform = file_stem
            .strip_prefix(&prefix)
            .ok_or_else(|| {
                invalid_data(format!(
                    "{} 文件名缺少 {prefix} 前缀",
                    fragment_path.display()
                ))
            })?
            .to_string();

        let manifest = release
            .get("manifest")
            .cloned()
            .ok_or_else(|| invalid_data(format!("{} 缺少 manifest", fragment_path.display())))?;
        let wasm = release
            .get("wasm")
            .cloned()
            .ok_or_else(|| invalid_data(format!("{} 缺少 wasm", fragment_path.display())))?;

        let key = format!("{id}@{version}");
        fragment_platforms
            .entry(key.clone())
            .or_default()
            .insert(platform);

        if let Some(existing) = releases.get_mut(&key) {
            for field in ["id", "name", "version", "description"] {
                if existing.get(field) != release.get(field) {
                    return Err(invalid_data(format!(
                        "插件 {key} 的 {field} 在不同平台不一致"
                    )));
                }
            }
            if existing.get("manifest") != Some(&manifest)
                || existing.get("wasm") != Some(&wasm)
                || existing.get("ui") != release.get("ui")
            {
                return Err(invalid_data(format!(
                    "插件 {key} 的 plugin.json、WASM 或 UI 在不同平台不一致"
                )));
            }
            merge_platform_map(existing, &release, "sidecars", &key)?;
            merge_platform_map(existing, &release, "signed_releases", &key)?;
        } else {
            releases.insert(key, release);
        }
    }

    if let Some(plugin) = expected_plugin {
        if releases.is_empty() {
            return Err(invalid_data(format!("没有找到插件 {plugin} 的目录片段")));
        }
        if releases.len() != 1 {
            return Err(invalid_data(format!(
                "插件 {plugin} 的输入包含多个版本，独立发布一次只能发布一个版本"
            )));
        }
    }

    let expected_platforms = std::env::var("TIANGONG_PLUGIN_EXPECTED_PLATFORMS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    for (key, platforms) in &fragment_platforms {
        if !expected_platforms.is_empty() && platforms != &expected_platforms {
            return Err(invalid_data(format!(
                "插件 {key} 平台不完整: expected={expected_platforms:?}, actual={platforms:?}"
            )));
        }
    }

    if let Some(plugin) = expected_plugin {
        copy_plugin_distribution_files(input_root, output_root, plugin)?;
    } else {
        copy_distribution_files(input_root, output_root)?;
    }
    let plugins = releases.into_values().collect::<Vec<_>>();
    let plugin_count = plugins.len();
    let catalog_path = output_root.join("plugins-index/catalog.json");
    let catalog = serde_json::json!({"version": PLUGIN_CATALOG_VERSION, "plugins": plugins});
    validate_plugin_catalog_value(&catalog, &catalog_path)?;
    write_json(&catalog_path, &catalog)?;
    eprintln!(
        "[xtask] 已合并 {} 个插件的多平台 OSS 制品: {}",
        plugin_count,
        output_root.display()
    );
    Ok(())
}

fn collect_fragment_files(directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_fragment_files(&path, output)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("json")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|value| value.to_str())
                == Some("fragments")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn required_json_string<'a>(
    value: &'a serde_json::Value,
    field: &str,
    path: &Path,
) -> io::Result<&'a str> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_data(format!("{} 缺少 {field}", path.display())))
}

fn merge_platform_map(
    existing: &mut serde_json::Value,
    incoming: &serde_json::Value,
    field: &str,
    key: &str,
) -> io::Result<()> {
    let incoming = match incoming[field].as_object() {
        Some(map) => map,
        // sidecars 为 null（无 sidecar 插件）时无需合并。
        None => return Ok(()),
    };
    let existing = existing[field]
        .as_object_mut()
        .ok_or_else(|| invalid_data(format!("插件 {key} 的 {field} 无效")))?;
    for (platform, artifact) in incoming {
        if existing
            .insert(platform.clone(), artifact.clone())
            .is_some()
        {
            return Err(invalid_data(format!("插件 {key} 的平台 {platform} 重复")));
        }
    }
    Ok(())
}

fn copy_distribution_files(input_root: &Path, output_root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(input_root)? {
        let entry = entry?;
        let source = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.file_name() == "plugins" {
            merge_directory(&source, &output_root.join("plugins"))?;
        } else {
            copy_distribution_files(&source, output_root)?;
        }
    }
    Ok(())
}

fn merge_directory(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            merge_directory(&source_path, &destination_path)?;
        } else if destination_path.exists() {
            if std::fs::read(&source_path)? != std::fs::read(&destination_path)? {
                return Err(invalid_data(format!(
                    "多平台制品内容冲突: {}",
                    destination_path.display()
                )));
            }
        } else {
            std::fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn copy_plugin_distribution_files(
    input_root: &Path,
    output_root: &Path,
    plugin: &str,
) -> io::Result<()> {
    for entry in std::fs::read_dir(input_root)? {
        let entry = entry?;
        let source = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if entry.file_name() == "plugins" {
            let plugin_source = source.join(plugin);
            if plugin_source.is_dir() {
                merge_directory(&plugin_source, &output_root.join("plugins").join(plugin))?;
            }
        } else {
            copy_plugin_distribution_files(&source, output_root, plugin)?;
        }
    }
    Ok(())
}

fn merge_plugin_catalog(
    current_catalog: &Path,
    plugin_release: &Path,
    output_catalog: &Path,
) -> io::Result<()> {
    let incoming_root: serde_json::Value = serde_json::from_slice(&std::fs::read(plugin_release)?)
        .map_err(|error| {
            invalid_data(format!("解析 {} 失败: {error}", plugin_release.display()))
        })?;
    validate_plugin_catalog_value(&incoming_root, plugin_release)?;
    let incoming_plugins = incoming_root
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_data("插件发布目录缺少 plugins"))?;
    if incoming_plugins.len() != 1 {
        return Err(invalid_data("独立插件发布目录必须只包含一个插件"));
    }
    let incoming = incoming_plugins[0].clone();
    let incoming_id = required_json_string(&incoming, "id", plugin_release)?.to_string();
    let incoming_version = required_json_string(&incoming, "version", plugin_release)?.to_string();
    semver::Version::parse(&incoming_version).map_err(|error| {
        invalid_data(format!(
            "插件 {incoming_id} 版本 {incoming_version} 无效: {error}"
        ))
    })?;

    let mut plugins = BTreeMap::<String, serde_json::Value>::new();
    if current_catalog != Path::new("-") && current_catalog.is_file() {
        let current: serde_json::Value = serde_json::from_slice(&std::fs::read(current_catalog)?)
            .map_err(|error| {
            invalid_data(format!("解析 {} 失败: {error}", current_catalog.display()))
        })?;
        if current.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid_data("当前插件目录版本无效"));
        }
        for release in current
            .get("plugins")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid_data("当前插件目录缺少 plugins"))?
        {
            let id = required_json_string(release, "id", current_catalog)?.to_string();
            if plugins.insert(id.clone(), release.clone()).is_some() {
                return Err(invalid_data(format!("当前插件目录包含重复 ID: {id}")));
            }
        }
    }
    plugins.insert(incoming_id.clone(), incoming);
    if let Some(parent) = output_catalog.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let merged = serde_json::json!({
        "version": PLUGIN_CATALOG_VERSION,
        "plugins": plugins.into_values().collect::<Vec<_>>()
    });
    validate_plugin_catalog_value(&merged, output_catalog)?;
    write_json(output_catalog, &merged)?;
    eprintln!(
        "[xtask] 已将插件 {incoming_id}@{incoming_version} 合并到目录: {}",
        output_catalog.display()
    );
    Ok(())
}
