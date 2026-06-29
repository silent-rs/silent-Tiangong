use anyhow::{Context, Result, anyhow};

use tiangong_core::model::SingleProviderClient;
use tiangong_core::models_config::{ModelCapability, ModelEntry, ModelsConfig, RoutingSlot};

use crate::args::{ModelArgs, ModelSubcommand, RouteSubcommand};

pub(crate) fn run_model_command(args: ModelArgs) -> Result<()> {
    let mut config = ModelsConfig::load();
    match args.command {
        ModelSubcommand::List { scope } => {
            print_list(&config, scope.as_deref());
        }
        ModelSubcommand::AddProvider {
            name,
            protocol,
            base_url,
            api_key,
            api_key_env,
            timeout_ms,
        } => {
            let api_key = match (api_key, api_key_env) {
                (Some(key), None) => key,
                (None, Some(env_name)) => format!("${{{env_name}}}"),
                _ => {
                    return Err(anyhow!(
                        "请指定 --api-key（明文）或 --api-key-env（环境变量名），二者互斥"
                    ));
                }
            };
            config.upsert_provider(&name, &base_url, &api_key, protocol, timeout_ms);
            config.save()?;
            println!("已保存供应商 {name}");
        }
        ModelSubcommand::RemoveProvider { name, force } => {
            if !config.providers.contains_key(&name) {
                return Err(anyhow!("供应商 {name} 不存在"));
            }
            let refs = config.provider_referenced_by(&name);
            if !refs.models.is_empty() || !refs.routes.is_empty() {
                if !force {
                    eprintln!("供应商 {name} 被以下配置引用：");
                    if !refs.models.is_empty() {
                        eprintln!("  模型：{}", refs.models.join(", "));
                    }
                    if !refs.routes.is_empty() {
                        eprintln!("  路由：{}", refs.routes.join(", "));
                    }
                    return Err(anyhow!("请使用 --force 强制删除，或先移除引用"));
                }
                let removed = config.remove_provider_force(&name);
                config.save()?;
                println!("已强制删除供应商 {name}（连带移除 {removed} 项）");
                return Ok(());
            }
            config.providers.remove(&name);
            config.save()?;
            println!("已删除供应商 {name}");
        }
        ModelSubcommand::AddModel {
            name,
            provider,
            model_id,
            capability,
        } => {
            if !config.providers.contains_key(&provider) {
                return Err(anyhow!(
                    "供应商 {provider} 不存在，请先 `tiangong model add-provider {provider} ...`"
                ));
            }
            let capabilities = parse_capabilities(&capability)?;
            config.upsert_model(&name, &provider, &model_id, capabilities.clone());
            config.save()?;
            let cap_str = if capabilities.is_empty() {
                "（无显式能力）".to_string()
            } else {
                capabilities
                    .iter()
                    .map(|c| c.key())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            println!(
                "已保存模型 {name}（provider={provider}, model_id={model_id}, capability={cap_str}）"
            );
        }
        ModelSubcommand::RemoveModel { name } => {
            let (removed, dangling) = config.remove_model(&name);
            if !removed {
                return Err(anyhow!("模型 {name} 不存在"));
            }
            config.save()?;
            if dangling.is_empty() {
                println!("已删除模型 {name}");
            } else {
                println!(
                    "已删除模型 {name}（注意以下路由变为悬空：{}）",
                    dangling.join(",")
                );
            }
        }
        ModelSubcommand::Configure => {
            super::configure::run_model_configure(&mut config)?;
        }
        ModelSubcommand::Route { command } => match command {
            RouteSubcommand::List => print_routes(&config),
            RouteSubcommand::Set { capability, model } => {
                let slot = parse_slot(&capability)?;
                config
                    .set_route_by_name(slot, &model)
                    .map_err(|e| anyhow!(e))?;
                config.save()?;
                println!("已设置路由 {capability} -> {model}");
            }
        },
        ModelSubcommand::Validate => {
            validate(&config)?;
            println!("模型配置校验通过");
        }
        ModelSubcommand::Test { target } => {
            test_model(&config, target.as_deref())?;
        }
    }
    Ok(())
}

fn print_list(config: &ModelsConfig, scope: Option<&str>) {
    match scope {
        Some("providers") => print_providers(config),
        Some("models") => print_models(config),
        Some("routes") => print_routes(config),
        Some(other) => {
            eprintln!("无效的范围：{other}（可用 providers / models / routes）");
        }
        None => {
            print_providers(config);
            println!();
            print_models(config);
            println!();
            print_routes(config);
        }
    }
}

fn print_providers(config: &ModelsConfig) {
    println!("== Providers ({}) ==", config.providers.len());
    if config.providers.is_empty() {
        println!("（无）");
        return;
    }
    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();
    for name in names {
        let p = &config.providers[name];
        println!(
            "{name}  protocol={} base_url={} timeout_ms={}",
            p.protocol.as_str(),
            p.base_url,
            p.timeout_ms
        );
    }
}

fn print_models(config: &ModelsConfig) {
    println!("== Models ({}) ==", config.models.len());
    if config.models.is_empty() {
        println!("（无）");
        return;
    }
    let mut names: Vec<&String> = config.models.keys().collect();
    names.sort();
    for name in names {
        let m = &config.models[name];
        let caps = m
            .capabilities
            .iter()
            .map(|c| c.key())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{name}  provider={} model={} capability={}",
            m.provider, m.model, caps
        );
    }
}

fn print_routes(config: &ModelsConfig) {
    println!("== Routing ==");
    if config.routing.is_empty() {
        println!("（无）");
        return;
    }
    // RoutingSlot 未实现 Ord，按 RoutingSlot::all() 的固定顺序输出
    for slot in RoutingSlot::all() {
        if let Some(entry) = config.routing.get(slot) {
            println!("{}  ->  {} ({})", slot.key(), entry.model, entry.provider);
        }
    }
}

fn validate(config: &ModelsConfig) -> Result<()> {
    let mut errors = Vec::new();

    // 检查路由引用的 provider 是否存在
    for (slot, entry) in &config.routing {
        if !config.providers.contains_key(&entry.provider) {
            errors.push(format!(
                "路由 {} 引用了不存在的 provider {}",
                slot.key(),
                entry.provider
            ));
        }
    }

    // 检查 models 注册项引用的 provider 是否存在
    for (name, entry) in &config.models {
        if !config.providers.contains_key(&entry.provider) {
            errors.push(format!(
                "模型 {name} 引用了不存在的 provider {}",
                entry.provider
            ));
        }
    }

    // chat 路由建议配置
    if !config.has_chat() {
        errors.push("未配置 chat 路由（建议 `tiangong model route set chat <model>`）".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        for e in &errors {
            eprintln!("❌ {e}");
        }
        Err(anyhow!("模型配置校验未通过（{} 个问题）", errors.len()))
    }
}

fn test_model(config: &ModelsConfig, target: Option<&str>) -> Result<()> {
    // target 为 capability/槽位（如 chat）或模型名；默认 chat
    let target = target.unwrap_or("chat");
    let provider_config = if let Some(slot) = RoutingSlot::from_key(target) {
        // 作为路由槽位解析
        match slot {
            RoutingSlot::Chat => config.to_chat_provider_config(),
            RoutingSlot::Lite => config.to_lite_provider_config(),
            _ => {
                // 其他槽位（multimodal/embedding 等）尝试按槽位解析
                let resolved = config
                    .resolve_slot(slot)
                    .ok_or_else(|| anyhow!("路由槽位 {target} 未配置"))?;
                build_provider_config_from_resolved(&resolved)
            }
        }
    } else {
        // 作为模型名解析
        let entry: &ModelEntry = config
            .models
            .get(target)
            .ok_or_else(|| anyhow!("模型 {target} 不存在，也不是有效路由槽位"))?;
        let provider = config
            .providers
            .get(&entry.provider)
            .ok_or_else(|| anyhow!("模型 {target} 的 provider {} 不存在", entry.provider))?;
        let resolved_api_key = ModelsConfig::resolve_api_key(&provider.api_key);
        build_provider_config(provider, entry, resolved_api_key)
    };

    println!("正在测试 {target} 连通性...");
    // 请求前检查 API Key 非空（${ENV} 未设置会解析为空串，避免无效请求）
    if provider_config.api_auth_token.trim().is_empty() {
        return Err(anyhow!(
            "API Key 为空，可能是环境变量未设置。请检查 models.json 中的 api_key 或设置对应环境变量"
        ));
    }
    let models =
        SingleProviderClient::list_models(&provider_config).context("模型连通性测试失败")?;
    println!("✅ 连通成功，返回 {} 个模型", models.len());
    if !models.is_empty() {
        let preview: Vec<&str> = models.iter().take(10).map(|s| s.as_str()).collect();
        println!("前 {} 个：{}", preview.len(), preview.join(", "));
    }
    Ok(())
}

/// 从 ProviderConfig + ModelEntry 构建单次测试用的 ModelProviderConfig。
fn build_provider_config(
    provider: &tiangong_core::models_config::ProviderConfig,
    entry: &ModelEntry,
    api_key: String,
) -> tiangong_core::model::ModelProviderConfig {
    tiangong_core::model::ModelProviderConfig {
        api_auth_token: api_key,
        api_base_url: provider.base_url.clone(),
        api_timeout_ms: provider.timeout_ms.to_string(),
        api_protocol: provider.protocol,
        api_model: entry.model.clone(),
        api_lite_model: String::new(),
    }
}

/// 从 ResolvedModel 构建 ModelProviderConfig。
fn build_provider_config_from_resolved(
    resolved: &tiangong_core::models_config::ResolvedModel,
) -> tiangong_core::model::ModelProviderConfig {
    tiangong_core::model::ModelProviderConfig {
        api_auth_token: resolved.api_key.clone(),
        api_base_url: resolved.base_url.clone(),
        api_timeout_ms: resolved.timeout_ms.to_string(),
        api_protocol: resolved.protocol,
        api_model: resolved.model.clone(),
        api_lite_model: String::new(),
    }
}

fn parse_capabilities(raw: &[String]) -> Result<Vec<ModelCapability>> {
    let mut result = Vec::new();
    for item in raw {
        let cap = ModelCapability::from_key(item).ok_or_else(|| anyhow!("无效的能力 {item}"))?;
        if !result.contains(&cap) {
            result.push(cap);
        }
    }
    Ok(result)
}

fn parse_slot(raw: &str) -> Result<RoutingSlot> {
    RoutingSlot::from_key(raw)
        .ok_or_else(|| anyhow!("无效的路由槽位 {raw}（可用 chat/lite/multimodal/image_generation/video_generation/stt/tts/embedding/rerank）"))
}
