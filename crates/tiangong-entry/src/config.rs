use anyhow::{Result, anyhow};

use tiangong_config::io::{custom_prompt_path, load_custom_prompt};
use tiangong_config::{default_tiangong_dir, load_server_config};
use tiangong_memory::{MemoryConfig, is_memory_disabled};

use crate::args::{ConfigArgs, ConfigSubcommand};

pub(crate) fn run_config_command(args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigSubcommand::Path => {
            print_paths();
            Ok(())
        }
        ConfigSubcommand::Show => {
            print_overview();
            Ok(())
        }
        ConfigSubcommand::Validate => validate(),
    }
}

fn print_paths() {
    let root = default_tiangong_dir();
    println!("配置目录：    {}", root.display());
    println!("模型配置：    {}", root.join("models.json").display());
    println!("MCP 配置：    {}", root.join("mcp.json").display());
    println!("Server 配置： {}", root.join("server.json").display());
    println!(
        "Memory 配置： {}",
        root.join("memory").join("config.json").display()
    );
    println!("自定义 Prompt：{}", custom_prompt_path().display());
    println!("Skill 目录：  {}", root.join("skills").display());
    println!("Skill 配置：  {}", root.join("skills.json").display());
}

/// 直接读取 JSON 文件并按数组/对象计数的轻量工具，避免依赖完整 TiangongState。
fn count_json_entries(path: &std::path::Path, key: &str) -> Option<usize> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value.get(key).and_then(|v| v.as_array()).map(|a| a.len())
}

/// 统计 ~/.tiangong/skills/ 下含 skill.toml 的目录数量（文件系统注册表事实源）。
///
/// Skill 注册表已从 skills.json.installed 迁移到文件系统目录存在性，
/// 因此扫描目录比读 JSON 更准确。目录不存在时返回 0。
fn count_skill_dirs(root: &std::path::Path) -> usize {
    let skills_dir = root.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|entry| {
            let path = entry.path();
            path.is_dir() && path.join("skill.toml").exists()
        })
        .count()
}

fn print_overview() {
    let root = default_tiangong_dir();
    let models = tiangong_config::io::load_models_config_at(&tiangong_config::io::storage_root());
    let server = load_server_config();
    let memory = MemoryConfig::load_or_default();

    // 模型
    let chat_model = models
        .resolve_slot(tiangong_llm::models_config::RoutingSlot::Chat)
        .map(|r| r.model);
    match &chat_model {
        Some(m) => println!("模型配置：✅ 已配置 chat={m}"),
        None => println!("模型配置：❌ 未配置 chat 路由"),
    }

    // Server
    let token_status = match &server.auth_token {
        Some(t) if !t.trim().is_empty() => "Token 已配置",
        _ => "Token 未配置",
    };
    println!(
        "Server：   {}:{}，{}",
        server.host, server.port, token_status
    );

    // Memory
    let memory_model = memory
        .model
        .as_ref()
        .map(|m| m.model.clone())
        .unwrap_or_else(|| "未配置".to_string());
    let memory_state = if is_memory_disabled() {
        "已禁用"
    } else {
        "已启用"
    };
    println!("Memory：   {memory_state}，LLM={memory_model}");

    // MCP（直接读 mcp.json 的 servers 数组长度）
    let mcp_count = count_json_entries(&root.join("mcp.json"), "servers").unwrap_or(0);
    println!("MCP：      {mcp_count} 个服务");

    // Skill（扫描 ~/.tiangong/skills/<id>/ 下含 skill.toml 的目录，文件系统注册表事实源）
    let skill_count = count_skill_dirs(&root);
    println!("Skill：    {skill_count} 个可用");

    // 自定义 Prompt（直接读 custom-prompt.md）
    let prompt = load_custom_prompt("").unwrap_or_default();
    if prompt.trim().is_empty() {
        println!("自定义 Prompt：未配置");
    } else {
        let chars = prompt.chars().count();
        println!("自定义 Prompt：已配置，{chars} 字");
    }
}

fn validate() -> Result<()> {
    let mut issues = Vec::new();

    // 模型配置
    let models = tiangong_config::io::load_models_config_at(&tiangong_config::io::storage_root());
    if models.is_empty() {
        issues.push("models.json 为空（未配置任何 provider 或路由）".to_string());
    } else {
        for (slot, entry) in &models.routing {
            if !models.providers.contains_key(&entry.provider) {
                issues.push(format!(
                    "路由 {} 引用了不存在的 provider {}",
                    slot.key(),
                    entry.provider
                ));
            }
        }
        for (name, entry) in &models.models {
            if !models.providers.contains_key(&entry.provider) {
                issues.push(format!(
                    "模型 {name} 引用了不存在的 provider {}",
                    entry.provider
                ));
            }
        }
    }

    // Server 配置
    let server = load_server_config();
    if server.port == 0 {
        issues.push("server.json 端口为 0".to_string());
    }

    // Memory 配置
    let memory = MemoryConfig::load_or_default();
    if let Some(m) = &memory.model
        && (m.base_url.trim().is_empty()
            || m.api_key.trim().is_empty()
            || m.model.trim().is_empty())
    {
        issues.push("memory LLM 端点配置不完整".to_string());
    }

    if issues.is_empty() {
        println!("✅ 配置校验通过");
        Ok(())
    } else {
        for issue in &issues {
            eprintln!("❌ {issue}");
        }
        Err(anyhow!("配置校验未通过（{} 个问题）", issues.len()))
    }
}
