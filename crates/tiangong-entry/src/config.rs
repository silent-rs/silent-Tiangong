use anyhow::{Result, anyhow};

use tiangong_config::{default_tiangong_dir, load_server_config};
use tiangong_core::app_state::TiangongState;
use tiangong_core::custom_prompt::custom_prompt_path;
use tiangong_core::models_config::ModelsConfig;
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
    println!("主配置：      {}", root.join("app.json").display());
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

fn print_overview() {
    let models = ModelsConfig::load();
    let server = load_server_config();
    let memory = MemoryConfig::load_or_default();
    let state = TiangongState::load_or_default();

    // 模型
    let chat_model = models
        .resolve_slot(tiangong_core::models_config::RoutingSlot::Chat)
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

    // MCP（从 state 读取已启用的 server 数）
    let mcp_count = state.agent_config().mcp.servers.len();
    println!("MCP：      {mcp_count} 个服务");

    // Skill
    let skills = state.installed_skills();
    println!("Skill：    {} 个可用", skills.len());

    // 自定义 Prompt
    let prompt = state.custom_system_prompt();
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
    let models = ModelsConfig::load();
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
