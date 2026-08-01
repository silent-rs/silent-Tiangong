use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::args::{MemoryArgs, MemoryConfigSubcommand, MemorySubcommand};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemorySelection {
    pub model_key: Option<String>,
    pub embedding_key: Option<String>,
    pub rerank_key: Option<String>,
    #[serde(default = "default_vector_mode")]
    pub vector_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MemoryUiModel {
    pub key: String,
    pub provider: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub dimension: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MemoryBootstrap {
    pub config: MemorySelection,
    pub models: Vec<MemoryUiModel>,
    #[serde(default)]
    pub disabled: bool,
}

pub(crate) fn run_memory_command(args: MemoryArgs) -> Result<()> {
    match args.command {
        MemorySubcommand::Config { command } => run_config(command),
        MemorySubcommand::Configure => super::configure::run_memory_configure(),
        MemorySubcommand::Enable => {
            set_enabled(true)?;
            println!("Memory 已启用");
            Ok(())
        }
        MemorySubcommand::Disable => {
            set_enabled(false)?;
            println!("Memory 已禁用");
            Ok(())
        }
        MemorySubcommand::Status => {
            print_status(&query_status()?);
            Ok(())
        }
        MemorySubcommand::Test => test_memory(),
    }
}

fn run_config(command: MemoryConfigSubcommand) -> Result<()> {
    match command {
        MemoryConfigSubcommand::Show => print_selection(&load_bootstrap()?),
        MemoryConfigSubcommand::Set {
            llm,
            embedding,
            rerank,
        } => {
            if llm.is_none() && embedding.is_none() && rerank.is_none() {
                return Err(anyhow!("请至少指定 --llm / --embedding / --rerank 之一"));
            }
            let mut bootstrap = load_bootstrap()?;
            if let Some(key) = llm {
                validate_model_key(&bootstrap, &key, "chat")?;
                bootstrap.config.model_key = Some(key);
            }
            if let Some(key) = embedding {
                validate_model_key(&bootstrap, &key, "embedding")?;
                bootstrap.config.embedding_key = Some(key);
            }
            if let Some(key) = rerank {
                validate_model_key(&bootstrap, &key, "rerank")?;
                bootstrap.config.rerank_key = Some(key);
            }
            save_selection(&bootstrap.config)?;
            println!("Memory 配置已更新");
        }
    }
    Ok(())
}

pub(crate) fn load_bootstrap() -> Result<MemoryBootstrap> {
    serde_json::from_value(invoke("ui.memory.config.get", serde_json::json!({}))?)
        .with_context(|| "解析 Memory 配置响应失败")
}

pub(crate) fn save_selection(selection: &MemorySelection) -> Result<()> {
    invoke("ui.memory.config.set", serde_json::to_value(selection)?)?;
    Ok(())
}

pub(crate) fn set_enabled(enabled: bool) -> Result<()> {
    let operation = if enabled { "enable" } else { "disable" };
    invoke(operation, serde_json::json!({}))?;
    Ok(())
}

pub(crate) fn query_status() -> Result<serde_json::Value> {
    invoke("status", serde_json::json!({}))
}

pub(crate) fn test_config() -> Result<serde_json::Value> {
    invoke("test", serde_json::json!({}))
}

fn invoke(operation: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
    tiangong_plugin_runtime::registry::invoke_sidecar(
        &tiangong_config::io::storage_root(),
        "memory",
        operation,
        payload,
    )
}

fn validate_model_key(bootstrap: &MemoryBootstrap, key: &str, capability: &str) -> Result<()> {
    let model = bootstrap
        .models
        .iter()
        .find(|model| model.key == key)
        .ok_or_else(|| anyhow!("模型 {key} 不存在于 models.json"))?;
    if !model.capabilities.iter().any(|item| item == capability) {
        bail!("模型 {key} 不具备 {capability} 能力");
    }
    Ok(())
}

fn print_selection(bootstrap: &MemoryBootstrap) {
    println!("== Memory 配置 ==");
    println!(
        "启用状态：{}",
        if bootstrap.disabled {
            "已禁用"
        } else {
            "已启用"
        }
    );
    println!("vector_mode: {}", bootstrap.config.vector_mode);
    println!(
        "LLM 模型：{}",
        bootstrap.config.model_key.as_deref().unwrap_or("未配置")
    );
    println!(
        "Embedding 模型：{}",
        bootstrap
            .config
            .embedding_key
            .as_deref()
            .unwrap_or("未配置")
    );
    println!(
        "Rerank 模型：{}",
        bootstrap.config.rerank_key.as_deref().unwrap_or("未配置")
    );
}

fn print_status(status: &serde_json::Value) {
    let disabled = status
        .get("disabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    println!("启用状态：{}", if disabled { "已禁用" } else { "已启用" });
    println!(
        "vector_mode: {}",
        status
            .get("vector_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未知")
    );
    for (key, label) in [
        ("llm", "LLM"),
        ("embedding", "Embedding"),
        ("rerank", "Rerank"),
    ] {
        let model = status
            .get(key)
            .and_then(|value| value.get("model"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未配置");
        println!("{label}：{model}");
    }
}

fn test_memory() -> Result<()> {
    let result = test_config()?;
    let ok = result
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if ok {
        println!("Memory 配置测试通过");
        return Ok(());
    }
    let issues = result
        .get("issues")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for issue in &issues {
        eprintln!("- {}", issue.as_str().unwrap_or("未知问题"));
    }
    Err(anyhow!("Memory 配置测试未通过（{} 个问题）", issues.len()))
}

fn default_vector_mode() -> String {
    "auto".to_string()
}
