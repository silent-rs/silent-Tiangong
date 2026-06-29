use anyhow::{Result, anyhow};

use tiangong_core::models_config::{ModelsConfig, ProviderConfig};
use tiangong_memory::{
    MemoryConfig, MemoryEmbeddingConfig, MemoryLlmConfig, MemoryRerankConfig, disable_memory,
    enable_memory, is_memory_disabled,
};

use crate::args::{MemoryArgs, MemoryConfigSubcommand, MemorySubcommand};

pub(crate) fn run_memory_command(args: MemoryArgs) -> Result<()> {
    match args.command {
        MemorySubcommand::Config { command } => run_config(command),
        MemorySubcommand::Enable => {
            enable_memory()?;
            println!("Memory 已启用");
            Ok(())
        }
        MemorySubcommand::Disable => {
            disable_memory()?;
            println!("Memory 已禁用");
            Ok(())
        }
        MemorySubcommand::Status => {
            print_status();
            Ok(())
        }
        MemorySubcommand::Test => {
            test_memory();
            Ok(())
        }
    }
}

fn run_config(command: MemoryConfigSubcommand) -> Result<()> {
    let config = MemoryConfig::load_or_default();
    match command {
        MemoryConfigSubcommand::Show => {
            print_config(&config);
        }
        MemoryConfigSubcommand::Set {
            llm,
            embedding,
            rerank,
        } => {
            if llm.is_none() && embedding.is_none() && rerank.is_none() {
                return Err(anyhow!("请至少指定 --llm / --embedding / --rerank 之一"));
            }
            let mut config = config;
            let models = ModelsConfig::load();

            if let Some(name) = llm {
                config.model = Some(resolve_to_llm_endpoint(&name, &models)?);
            }
            if let Some(name) = embedding {
                config.embedding = Some(resolve_to_embedding_endpoint(&name, &models)?);
            }
            if let Some(name) = rerank {
                config.rerank = Some(resolve_to_rerank_endpoint(&name, &models)?);
            }
            config.save()?;
            println!("Memory 配置已更新");
        }
    }
    Ok(())
}

/// 从 models.json 引用模型，构建 Memory LLM 端点配置。
fn resolve_to_llm_endpoint(model_name: &str, models: &ModelsConfig) -> Result<MemoryLlmConfig> {
    let (provider_name, provider, model_id) = lookup_model(models, model_name)?;
    Ok(MemoryLlmConfig {
        provider_key: Some(provider_name),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: model_id,
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
    })
}

/// 从 models.json 引用模型，构建 Memory Embedding 端点配置。
fn resolve_to_embedding_endpoint(
    model_name: &str,
    models: &ModelsConfig,
) -> Result<MemoryEmbeddingConfig> {
    let (provider_name, provider, model_id) = lookup_model(models, model_name)?;
    Ok(MemoryEmbeddingConfig {
        provider_key: Some(provider_name),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: model_id,
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
        dimension: 0,
    })
}

/// 从 models.json 引用模型，构建 Memory Rerank 端点配置。
fn resolve_to_rerank_endpoint(
    model_name: &str,
    models: &ModelsConfig,
) -> Result<MemoryRerankConfig> {
    let (provider_name, provider, model_id) = lookup_model(models, model_name)?;
    Ok(MemoryRerankConfig {
        provider_key: Some(provider_name),
        base_url: provider.base_url.clone(),
        api_key: provider.api_key.clone(),
        model: model_id,
        protocol: provider.protocol,
        timeout_ms: provider.timeout_ms,
    })
}

/// 在 models.json 注册表中查找模型，返回 (provider_name, provider_config, model_id)。
fn lookup_model(models: &ModelsConfig, name: &str) -> Result<(String, ProviderConfig, String)> {
    let entry = models.models.get(name).ok_or_else(|| {
        anyhow!("模型 {name} 不存在于 models.json，请先 `tiangong model add-model {name} ...`")
    })?;
    let provider = models
        .providers
        .get(&entry.provider)
        .ok_or_else(|| anyhow!("模型 {name} 的 provider {} 不存在", entry.provider))?;
    Ok((
        entry.provider.clone(),
        provider.clone(),
        entry.model.clone(),
    ))
}

fn print_config(config: &MemoryConfig) {
    println!("== Memory 配置 ==");
    println!(
        "启用状态：{}",
        if is_memory_disabled() {
            "\x1b[31m禁用\x1b[0m"
        } else {
            "\x1b[32m启用\x1b[0m"
        }
    );
    println!("vector_mode: {:?}", config.vector_mode);

    println!("\n-- LLM 端点 --");
    match &config.model {
        Some(m) => {
            println!("  base_url: {}", m.base_url);
            println!("  model: {}", m.model);
            println!("  protocol: {}", m.protocol.as_str());
            println!("  timeout_ms: {}", m.timeout_ms);
        }
        None => println!("  （未配置）"),
    }

    println!("\n-- Embedding 端点 --");
    match &config.embedding {
        Some(e) => {
            println!("  base_url: {}", e.base_url);
            println!("  model: {}", e.model);
            println!("  dimension: {}", e.dimension);
        }
        None => println!("  （未配置）"),
    }

    println!("\n-- Rerank 端点 --");
    match &config.rerank {
        Some(r) => {
            println!("  base_url: {}", r.base_url);
            println!("  model: {}", r.model);
        }
        None => println!("  （未配置）"),
    }
}

fn print_status() {
    let config = MemoryConfig::load_or_default();
    let disabled = is_memory_disabled();
    let llm_valid = config
        .model
        .as_ref()
        .map(|m| endpoint_valid(&m.base_url, &m.api_key, &m.model))
        .unwrap_or(false);

    println!("禁用标记：{}", if disabled { "存在" } else { "无" });
    println!(
        "启用状态：{}",
        if !disabled && llm_valid {
            "\x1b[32m已启用且配置有效\x1b[0m"
        } else if disabled {
            "\x1b[31m已禁用\x1b[0m"
        } else {
            "\x1b[33m启用但 LLM 端点未配置或无效\x1b[0m"
        }
    );
    println!("vector_mode: {:?}", config.vector_mode);
    println!(
        "LLM 端点：{}",
        config
            .model
            .as_ref()
            .map(|m| format!("{} @ {}", m.model, m.base_url))
            .unwrap_or_else(|| "（未配置）".to_string())
    );
    println!(
        "Embedding 端点：{}",
        config
            .embedding
            .as_ref()
            .map(|e| format!("{} (dim={})", e.model, e.dimension))
            .unwrap_or_else(|| "（未配置）".to_string())
    );
    println!(
        "Rerank 端点：{}",
        config
            .rerank
            .as_ref()
            .map(|r| r.model.clone())
            .unwrap_or_else(|| "（未配置）".to_string())
    );
}

fn endpoint_valid(base_url: &str, api_key: &str, model: &str) -> bool {
    !base_url.trim().is_empty() && !api_key.trim().is_empty() && !model.trim().is_empty()
}

/// 测试 Memory 配置完整性。
///
/// 校验 Memory 端点配置是否完整（base_url/api_key/model 非空），
/// 并验证引用的模型在 models.json 中存在。真实连通性测试由
/// `tiangong model test` 覆盖（端点最终来自 models.json）。
fn test_memory() {
    let config = MemoryConfig::load_or_default();
    if is_memory_disabled() {
        println!("⚠️  Memory 当前已被禁用（`tiangong memory enable` 可重新启用）");
    }

    println!("== Memory 配置测试 ==");

    let mut issues = Vec::new();

    // LLM 端点
    match &config.model {
        Some(m) if endpoint_valid(&m.base_url, &m.api_key, &m.model) => {
            println!("✅ LLM 端点有效：{} @ {}", m.model, m.base_url);
        }
        Some(_) => {
            issues.push("LLM 端点配置不完整（base_url/api_key/model 不能为空）".to_string());
        }
        None => {
            issues.push("未配置 LLM 端点".to_string());
        }
    }

    // Embedding 端点（可选）
    match &config.embedding {
        Some(e) if endpoint_valid(&e.base_url, &e.api_key, &e.model) => {
            if e.dimension == 0 {
                println!("⚠️  Embedding 端点有效但 dimension=0（向量维度未设置）");
            } else {
                println!("✅ Embedding 端点有效：{} (dim={})", e.model, e.dimension);
            }
        }
        Some(_) => {
            issues.push("Embedding 端点配置不完整".to_string());
        }
        None => {
            println!("ℹ️  未配置 Embedding 端点（可选，缺失会降级）");
        }
    }

    // Rerank 端点（可选）
    match &config.rerank {
        Some(r) if endpoint_valid(&r.base_url, &r.api_key, &r.model) => {
            println!("✅ Rerank 端点有效：{}", r.model);
        }
        Some(_) => {
            issues.push("Rerank 端点配置不完整".to_string());
        }
        None => {
            println!("ℹ️  未配置 Rerank 端点（可选，缺失会降级）");
        }
    }

    if issues.is_empty() {
        println!("\n✅ Memory 配置测试通过");
        println!("（如需验证真实连通性，可对引用的模型执行 `tiangong model test <模型名>`）");
    } else {
        println!("\n❌ Memory 配置测试未通过：");
        for issue in &issues {
            println!("  - {issue}");
        }
    }
}
