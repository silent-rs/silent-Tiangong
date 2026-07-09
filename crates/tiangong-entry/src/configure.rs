//! 交互式配置向导实现。
//!
//! 为 model / server / memory 三个模块提供可选的交互式引导，
//! 收集用户输入后调用现有配置方法落盘。非 TTY 环境由调用前的
//! `ensure_terminal()` 拦截，不会卡住脚本/CI。

use anyhow::{Result, anyhow};

use tiangong_config::{generate_token, load_server_config, save_server_config};
use tiangong_core::model::ProviderProtocol;
use tiangong_core::models_config::{ModelCapability, ModelsConfig, RoutingSlot};
use tiangong_memory::{MemoryConfig, enable_memory};

use crate::interactive as ui;

/// 模型配置向导：引导完成 provider → model → route 三步。
pub fn run_model_configure(config: &mut ModelsConfig) -> Result<()> {
    ui::ensure_terminal()?;
    println!("=== 模型配置向导 ===\n");

    // ── 步骤 1：Provider ──
    let (protocol, provider_name, base_url, api_key) = prompt_provider(config)?;
    config.upsert_provider(&provider_name, &base_url, &api_key, protocol, 60_000);
    println!();

    // ── 步骤 2：Model ──
    let (model_name, model_id, capabilities) = prompt_model(&provider_name, protocol)?;
    config.upsert_model(&model_name, &provider_name, &model_id, capabilities.clone());
    println!();

    // ── 步骤 3：Route ──
    prompt_route(config, &model_name, &capabilities)?;
    println!();

    // ── 落盘 ──
    let dir = tiangong_config::io::storage_root();
    tiangong_config::io::save_models_config_at(&dir, config)?;
    println!("✅ 模型配置已保存");
    println!("提示：可用 `tiangong model test chat` 验证连通性，或 `tiangong doctor` 检查整体环境");
    Ok(())
}

/// 收集 Provider 信息：协议、名称、base_url、api_key。
fn prompt_provider(_config: &ModelsConfig) -> Result<(ProviderProtocol, String, String, String)> {
    let protocols = ["DeepSeek", "OpenAI 兼容", "Anthropic"];
    let idx = ui::select("选择模型协议", &protocols)?;
    let (protocol, default_name, default_url) = match idx {
        0 => (
            "deepseek".parse::<ProviderProtocol>().unwrap(),
            "deepseek",
            "https://api.deepseek.com",
        ),
        1 => (
            ProviderProtocol::OpenAiChatCompletions,
            "openai",
            "https://api.openai.com/v1",
        ),
        2 => (
            "anthropic".parse::<ProviderProtocol>().unwrap(),
            "anthropic",
            "https://api.anthropic.com",
        ),
        _ => unreachable!(),
    };

    let provider_name = ui::input("供应商名称（本地标识）", default_name)?;
    let provider_name = if provider_name.trim().is_empty() {
        default_name.to_string()
    } else {
        provider_name.trim().to_string()
    };

    let base_url = ui::input("API base URL", default_url)?;
    let base_url = if base_url.trim().is_empty() {
        default_url.to_string()
    } else {
        base_url.trim().to_string()
    };

    // api_key 方式
    let key_modes = [
        "环境变量名（推荐，写入为 ${VAR} 模板）",
        "明文输入（不推荐）",
    ];
    let mode = ui::select("API Key 输入方式", &key_modes)?;
    let api_key = if mode == 0 {
        let env_name = ui::input_required("请输入环境变量名（如 DEEPSEEK_API_KEY）")?;
        format!("${{{}}}", env_name.trim())
    } else {
        let plain = ui::password("请输入 API Key")?;
        if plain.trim().is_empty() {
            return Err(anyhow!("API Key 不能为空"));
        }
        plain.trim().to_string()
    };

    Ok((protocol, provider_name, base_url, api_key))
}

/// 收集 Model 信息：别名、model_id、capability。
fn prompt_model(
    provider_name: &str,
    protocol: ProviderProtocol,
) -> Result<(String, String, Vec<ModelCapability>)> {
    let default_id = match protocol {
        ProviderProtocol::DeepSeek => "deepseek-chat",
        ProviderProtocol::OpenAiChatCompletions => "gpt-4o-mini",
        ProviderProtocol::Anthropic => "claude-sonnet-4-20250514",
    };

    let default_alias = format!("{provider_name}-chat");
    let model_name = ui::input("模型别名（本地标识）", &default_alias)?;
    let model_name = if model_name.trim().is_empty() {
        default_alias
    } else {
        model_name.trim().to_string()
    };

    let model_id = ui::input("模型 ID（供应商侧标识）", default_id)?;
    let model_id = if model_id.trim().is_empty() {
        default_id.to_string()
    } else {
        model_id.trim().to_string()
    };

    // capability 多选
    let caps = [
        ("chat", ModelCapability::Chat),
        ("multimodal", ModelCapability::Multimodal),
        ("image_generation", ModelCapability::ImageGeneration),
        ("video_generation", ModelCapability::VideoGeneration),
        ("stt", ModelCapability::Stt),
        ("tts", ModelCapability::Tts),
        ("embedding", ModelCapability::Embedding),
        ("rerank", ModelCapability::Rerank),
    ];
    let cap_labels: Vec<&str> = caps.iter().map(|(k, _)| *k).collect();
    // 默认勾选 chat（caps[0]），降低误操作概率
    let selected =
        ui::multiselect_with_defaults("选择模型能力（空格切换，回车确认）", &cap_labels, &[0])?;
    let capabilities: Vec<ModelCapability> = selected.iter().map(|&i| caps[i].1).collect();

    // 至少要有 chat（最常见的对话场景）
    let capabilities = if capabilities.is_empty() {
        vec![ModelCapability::Chat]
    } else {
        capabilities
    };

    Ok((model_name, model_id, capabilities))
}

/// 收集路由槽位并设置（利用已有 capability 校验）。
fn prompt_route(
    config: &mut ModelsConfig,
    model_name: &str,
    capabilities: &[ModelCapability],
) -> Result<()> {
    if !ui::confirm("是否设置该模型为默认路由？", true)? {
        println!("已跳过路由设置（可稍后用 `tiangong model route set` 配置）");
        return Ok(());
    }

    // 根据模型能力推荐槽位
    let slots: Vec<(&str, RoutingSlot)> = vec![
        ("chat", RoutingSlot::Chat),
        ("lite", RoutingSlot::Lite),
        ("multimodal", RoutingSlot::Multimodal),
        ("image_generation", RoutingSlot::ImageGeneration),
        ("video_generation", RoutingSlot::VideoGeneration),
        ("stt", RoutingSlot::Stt),
        ("tts", RoutingSlot::Tts),
        ("embedding", RoutingSlot::Embedding),
        ("rerank", RoutingSlot::Rerank),
    ];
    // 默认推荐 chat
    let default_slot = if capabilities.contains(&ModelCapability::Chat) {
        0
    } else {
        capabilities
            .first()
            .and_then(|c| slots.iter().position(|(_, s)| s.capability() == Some(*c)))
            .unwrap_or(0)
    };

    let slot_labels: Vec<&str> = slots.iter().map(|(k, _)| *k).collect();
    let idx = ui::select_with_default("选择路由槽位", &slot_labels, default_slot)?;
    let slot = slots[idx].1;

    config
        .set_route_by_name(slot, model_name)
        .map_err(|e| anyhow!(e))?;
    println!("已设置路由 {} -> {model_name}", slot.key());
    Ok(())
}

// ── Server 配置向导 ──

/// Server 配置向导：引导设置监听地址与 Token。
pub fn run_server_configure() -> Result<()> {
    ui::ensure_terminal()?;
    println!("=== Server 配置向导 ===\n");

    let mut config = load_server_config();

    let host = ui::input("监听地址", &config.host)?;
    config.host = if host.trim().is_empty() {
        config.host
    } else {
        host.trim().to_string()
    };

    let port_input = ui::input("监听端口", &config.port.to_string())?;
    let port = port_input
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow!("端口必须是 1-65535 的整数，实际输入：{port_input}"))?;
    if port == 0 {
        return Err(anyhow!("端口不能为 0"));
    }
    config.port = port;

    // Token
    let token_options = [
        "生成随机 Token（推荐）",
        "手动输入 Token",
        "跳过（不设鉴权）",
    ];
    let token_mode = ui::select("鉴权 Token", &token_options)?;
    match token_mode {
        0 => {
            let len_input = ui::input("Token 长度", "32")?;
            let len: usize = len_input.trim().parse().unwrap_or(32);
            let token = generate_token(len);
            config.auth_token = Some(token);
            println!("已生成 Token：{}", config.masked_auth_token());
            println!("（完整 Token 已写入 server.json）");
        }
        1 => {
            let token = ui::password("请输入 Token")?;
            if token.trim().is_empty() {
                return Err(anyhow!("Token 不能为空；如不想设置鉴权请选择「跳过」"));
            }
            config.auth_token = Some(token.trim().to_string());
        }
        _ => {
            config.auth_token = None;
            println!("已跳过 Token 设置（接口将无鉴权）");
        }
    }

    save_server_config(&config)?;
    println!();
    println!("✅ Server 配置已保存：{}:{}", config.host, config.port);
    println!("提示：可用 `tiangong server status` 检查运行状态");
    Ok(())
}

// ── Memory 配置向导 ──

/// Memory 配置向导：引导选择 Memory 端点模型。
pub fn run_memory_configure() -> Result<()> {
    ui::ensure_terminal()?;
    println!("=== Memory 配置向导 ===\n");

    // 是否启用
    let currently_disabled = tiangong_memory::is_memory_disabled();
    if currently_disabled {
        if ui::confirm("Memory 当前已禁用，是否启用？", true)? {
            enable_memory()?;
            println!("Memory 已启用");
        } else {
            println!("已保持禁用状态，向导结束");
            return Ok(());
        }
    } else if !ui::confirm("Memory 当前已启用，继续配置端点？", true)? {
        println!("已跳过，向导结束");
        return Ok(());
    }

    let models = tiangong_config::io::load_models_config_at(&tiangong_config::io::storage_root());
    if models.models.is_empty() {
        println!("⚠️  models.json 中没有已注册模型，请先运行 `tiangong model configure`");
        println!("（Memory 端点引用 models.json 中的模型）");
        return Ok(());
    }

    let mut config = MemoryConfig::load_or_default();

    // LLM 模型（要求 chat 能力）
    config.model = Some(pick_memory_llm(&models)?);
    println!();

    // Embedding 模型（可选）
    if ui::confirm("是否配置 Embedding 端点？", false)?
        && let Some(m) = pick_memory_endpoint(&models, ModelCapability::Embedding, "Embedding")?
    {
        config.embedding = Some(m);
    }
    println!();

    // Rerank 模型（可选）
    if ui::confirm("是否配置 Rerank 端点？", false)?
        && let Some(m) = pick_memory_rerank(&models, "Rerank")?
    {
        config.rerank = Some(m);
    }

    config.save()?;
    println!();
    println!("✅ Memory 配置已保存");
    println!("提示：可用 `tiangong memory test` 检查端点有效性");
    Ok(())
}

/// 让用户从已注册模型中选择一个作为 Memory LLM（校验 chat 能力）。
fn pick_memory_llm(models: &ModelsConfig) -> Result<tiangong_memory::MemoryLlmConfig> {
    let mut chat_models: Vec<(&String, &str)> = models
        .models
        .iter()
        .filter(|(_, m)| m.capabilities.contains(&ModelCapability::Chat))
        .map(|(k, m)| (k, m.model.as_str()))
        .collect();
    chat_models.sort_by_key(|(k, _)| k.as_str());
    if chat_models.is_empty() {
        return Err(anyhow!(
            "没有具备 chat 能力的已注册模型，请先 `tiangong model add-model ... --capability chat`"
        ));
    }
    let labels: Vec<String> = chat_models
        .iter()
        .map(|(k, id)| format!("{k} ({id})"))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let idx = ui::select("选择 Memory LLM 模型", &label_refs)?;
    let (name, _) = chat_models[idx];
    build_llm_config(models, name)
}

/// 让用户从已注册模型中选择一个 Embedding 模型（校验对应能力），可跳过。
fn pick_memory_endpoint(
    models: &ModelsConfig,
    expected: ModelCapability,
    label: &str,
) -> Result<Option<tiangong_memory::MemoryEmbeddingConfig>> {
    let mut candidates: Vec<(&String, &str)> = models
        .models
        .iter()
        .filter(|(_, m)| m.capabilities.contains(&expected))
        .map(|(k, m)| (k, m.model.as_str()))
        .collect();
    candidates.sort_by_key(|(k, _)| k.as_str());
    if candidates.is_empty() {
        println!("⚠️  没有具备 {label} 能力的已注册模型，已跳过");
        return Ok(None);
    }
    let labels: Vec<String> = candidates
        .iter()
        .map(|(k, id)| format!("{k} ({id})"))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let idx = ui::select(&format!("选择 {label} 模型"), &label_refs)?;
    let (name, _) = candidates[idx];
    let provider_name = &models.models[name].provider;
    let provider = models
        .providers
        .get(provider_name)
        .ok_or_else(|| anyhow!("模型 {name} 引用了不存在的 provider {provider_name}"))?;
    use tiangong_memory::MemoryEmbeddingConfig;
    Ok(Some(MemoryEmbeddingConfig {
        provider_key: Some(provider_name.clone()),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: models.models[name].model.clone(),
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
        dimension: 0,
    }))
}

/// 让用户从已注册模型中选择一个 Rerank 模型（校验 rerank 能力），可跳过。
fn pick_memory_rerank(
    models: &ModelsConfig,
    label: &str,
) -> Result<Option<tiangong_memory::MemoryRerankConfig>> {
    let mut candidates: Vec<(&String, &str)> = models
        .models
        .iter()
        .filter(|(_, m)| m.capabilities.contains(&ModelCapability::Rerank))
        .map(|(k, m)| (k, m.model.as_str()))
        .collect();
    candidates.sort_by_key(|(k, _)| k.as_str());
    if candidates.is_empty() {
        println!("⚠️  没有具备 {label} 能力的已注册模型，已跳过");
        return Ok(None);
    }
    let labels: Vec<String> = candidates
        .iter()
        .map(|(k, id)| format!("{k} ({id})"))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let idx = ui::select(&format!("选择 {label} 模型"), &label_refs)?;
    let (name, _) = candidates[idx];
    let provider_name = &models.models[name].provider;
    let provider = models
        .providers
        .get(provider_name)
        .ok_or_else(|| anyhow!("模型 {name} 引用了不存在的 provider {provider_name}"))?;
    use tiangong_memory::MemoryRerankConfig;
    Ok(Some(MemoryRerankConfig {
        provider_key: Some(provider_name.clone()),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: models.models[name].model.clone(),
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
    }))
}

/// 构建 Memory LLM 配置（复用 provider 端点）。
fn build_llm_config(models: &ModelsConfig, name: &str) -> Result<tiangong_memory::MemoryLlmConfig> {
    let entry = &models.models[name];
    let provider = models
        .providers
        .get(&entry.provider)
        .ok_or_else(|| anyhow!("模型 {name} 引用了不存在的 provider {}", entry.provider))?;
    Ok(tiangong_memory::MemoryLlmConfig {
        provider_key: Some(entry.provider.clone()),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: entry.model.clone(),
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
    })
}
