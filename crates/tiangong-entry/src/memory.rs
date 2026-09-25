use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::args::{MemoryArgs, MemoryComponentSourceArg, MemoryConfigSubcommand, MemorySubcommand};

/// 与 memory 插件协议 `ui::MemorySelection` 同构（入口不直接依赖插件协议 crate）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemorySelection {
    #[serde(default)]
    pub local_tier: String,
    #[serde(default)]
    pub llm: MemoryLlmSelection,
    #[serde(default)]
    pub embedding: MemoryComponentSelection,
    #[serde(default)]
    pub rerank: MemoryComponentSelection,
    #[serde(default = "default_vector_mode")]
    pub vector_mode: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemoryLlmSelection {
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<MemoryRemoteSelection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemoryComponentSelection {
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<MemoryRemoteSelection>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MemoryRemoteSelection {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub has_api_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MemoryUiModel {
    pub key: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MemoryBootstrap {
    pub config: MemorySelection,
    pub models: Vec<MemoryUiModel>,
    #[serde(default)]
    pub default_llm: Option<String>,
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
            tier,
            embedding,
            embedding_url,
            embedding_model,
            embedding_dimension,
            embedding_key,
            rerank,
            rerank_url,
            rerank_model,
            rerank_key,
        } => {
            if llm.is_none() && tier.is_none() && embedding.is_none() && rerank.is_none() {
                return Err(anyhow!(
                    "请至少指定 --llm / --tier / --embedding / --rerank 之一"
                ));
            }
            let mut bootstrap = load_bootstrap()?;
            if let Some(key) = llm {
                let key = key.trim();
                bootstrap.config.llm = if key.is_empty() || key == "default" {
                    MemoryLlmSelection {
                        source: "models_ref".to_string(),
                        ..Default::default()
                    }
                } else {
                    validate_llm_key(&bootstrap, key)?;
                    MemoryLlmSelection {
                        source: "models_ref".to_string(),
                        key: Some(key.to_string()),
                        remote: None,
                    }
                };
            }
            if let Some(tier) = tier {
                bootstrap.config.local_tier = tier.key().to_string();
            }
            if let Some(source) = embedding {
                apply_component(
                    &mut bootstrap.config.embedding,
                    source,
                    RemoteArgs {
                        url: embedding_url,
                        model: embedding_model,
                        dimension: embedding_dimension,
                        api_key: embedding_key,
                    },
                    "Embedding",
                    true,
                )?;
            }
            if let Some(source) = rerank {
                apply_component(
                    &mut bootstrap.config.rerank,
                    source,
                    RemoteArgs {
                        url: rerank_url,
                        model: rerank_model,
                        dimension: None,
                        api_key: rerank_key,
                    },
                    "Rerank",
                    false,
                )?;
            }
            save_selection(&bootstrap.config)?;
            println!("Memory 配置已更新");
        }
    }
    Ok(())
}

struct RemoteArgs {
    url: Option<String>,
    model: Option<String>,
    dimension: Option<usize>,
    api_key: Option<String>,
}

fn apply_component(
    component: &mut MemoryComponentSelection,
    source: MemoryComponentSourceArg,
    args: RemoteArgs,
    label: &str,
    needs_dimension: bool,
) -> Result<()> {
    component.source = source.key().to_string();
    if source != MemoryComponentSourceArg::Remote {
        component.remote = None;
        return Ok(());
    }
    let mut remote = component.remote.clone().unwrap_or_default();
    if let Some(url) = args.url {
        remote.base_url = url;
    }
    if let Some(model) = args.model {
        remote.model = model;
    }
    if let Some(dimension) = args.dimension {
        remote.dimension = Some(dimension);
    }
    remote.api_key = args.api_key.filter(|value| !value.trim().is_empty());
    if remote.base_url.trim().is_empty() || remote.model.trim().is_empty() {
        bail!(
            "{label} 在线端点需要 --{}-url 与 --{}-model",
            label.to_lowercase(),
            label.to_lowercase()
        );
    }
    if needs_dimension && remote.dimension.unwrap_or(0) == 0 {
        bail!("Embedding 在线端点需要 --embedding-dimension");
    }
    component.remote = Some(remote);
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

fn validate_llm_key(bootstrap: &MemoryBootstrap, key: &str) -> Result<()> {
    if bootstrap.models.iter().any(|model| model.key == key) {
        return Ok(());
    }
    bail!("{key} 不是可用的 chat 模型或路由（可用 `tiangong memory config show` 查看候选）")
}

fn describe_component(component: &MemoryComponentSelection, tier: &str) -> String {
    match component.source.as_str() {
        "builtin" => format!("内置（档位 {tier}）"),
        "remote" => component
            .remote
            .as_ref()
            .map(|remote| {
                let dimension = remote
                    .dimension
                    .map(|value| format!(" / dim={value}"))
                    .unwrap_or_default();
                format!("在线 {} @ {}{}", remote.model, remote.base_url, dimension)
            })
            .unwrap_or_else(|| "在线（未填写）".to_string()),
        _ => "未启用".to_string(),
    }
}

fn print_selection(bootstrap: &MemoryBootstrap) {
    let config = &bootstrap.config;
    println!("== Memory 配置 ==");
    println!(
        "启用状态：{}",
        if bootstrap.disabled {
            "已禁用"
        } else {
            "已启用"
        }
    );
    println!("vector_mode: {}", config.vector_mode);
    println!("本地档位：{}", config.local_tier);
    let llm = match (config.llm.source.as_str(), config.llm.key.as_deref()) {
        ("remote", _) => config
            .llm
            .remote
            .as_ref()
            .map(|remote| format!("在线 {} @ {}", remote.model, remote.base_url))
            .unwrap_or_else(|| "在线（未填写）".to_string()),
        (_, Some(key)) => format!("{key}（指定）"),
        _ => format!(
            "跟随 lite → chat（当前：{}）",
            bootstrap.default_llm.as_deref().unwrap_or("未配置")
        ),
    };
    println!("LLM：{llm}");
    println!(
        "Embedding：{}",
        describe_component(&config.embedding, &config.local_tier)
    );
    println!(
        "Rerank：{}",
        describe_component(&config.rerank, &config.local_tier)
    );
    if !bootstrap.models.is_empty() {
        println!("\nLLM 可选：");
        for model in &bootstrap.models {
            let kind = if model.kind == "route" {
                "路由"
            } else {
                "模型"
            };
            println!(
                "  {} [{kind}] ({} / {})",
                model.key, model.provider, model.model
            );
        }
    }
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
        let entry = status.get(key);
        let source = entry
            .and_then(|value| value.get("source"))
            .and_then(serde_json::Value::as_str);
        let model = entry
            .and_then(|value| value.get("model"))
            .and_then(serde_json::Value::as_str);
        let text = match (source, model) {
            (Some("builtin"), _) => format!(
                "内置（档位 {}）",
                entry
                    .and_then(|value| value.get("tier"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("mid")
            ),
            (_, Some(model)) => model.to_string(),
            _ => "未配置".to_string(),
        };
        println!("{label}：{text}");
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
