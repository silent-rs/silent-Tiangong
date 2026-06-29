use anyhow::Result;

use tiangong_config::default_tiangong_dir;
use tiangong_config::load_server_config;
use tiangong_core::custom_prompt::custom_prompt_path;
use tiangong_core::models_config::{ModelsConfig, RoutingSlot};
use tiangong_memory::{MemoryConfig, is_memory_disabled};

use crate::args::DoctorArgs;

pub(crate) fn run_doctor_command(args: DoctorArgs) -> Result<()> {
    let mut report = DoctorReport::default();

    check_config_dir(&mut report);
    check_models(&mut report, args.deep);
    check_server(&mut report);
    check_server_token(&mut report);
    check_memory(&mut report);
    check_mcp(&mut report);
    check_skills(&mut report);
    check_custom_prompt(&mut report);

    report.print();
    Ok(())
}

#[derive(Default)]
struct DoctorReport {
    lines: Vec<String>,
    has_error: bool,
}

impl DoctorReport {
    fn ok(&mut self, label: &str, detail: impl Into<String>) {
        self.lines
            .push(format!("\x1b[32m✅\x1b[0m {label:<16} {}", detail.into()));
    }

    fn warn(&mut self, label: &str, detail: impl Into<String>) {
        self.lines
            .push(format!("\x1b[33m⚠️\x1b[0m  {label:<16} {}", detail.into()));
    }

    fn err(&mut self, label: &str, detail: impl Into<String>) {
        self.has_error = true;
        self.lines
            .push(format!("\x1b[31m❌\x1b[0m {label:<16} {}", detail.into()));
    }

    fn skip(&mut self, label: &str, detail: impl Into<String>) {
        self.lines
            .push(format!("\x1b[90m⏭️\x1b[0m {label:<16} {}", detail.into()));
    }

    fn blank(&mut self) {
        self.lines.push(String::new());
    }

    fn hint(&mut self, text: &str) {
        self.lines.push(format!("    {text}"));
    }

    fn print(&self) {
        for line in &self.lines {
            println!("{line}");
        }
    }
}

fn check_config_dir(report: &mut DoctorReport) {
    let dir = default_tiangong_dir();
    if dir.exists() {
        report.ok("配置目录", dir.display().to_string());
    } else {
        report.err("配置目录", format!("{} 不存在", dir.display()));
    }
}

fn check_models(report: &mut DoctorReport, deep: bool) {
    let models = ModelsConfig::load();
    let chat = models.resolve_slot(RoutingSlot::Chat);

    match &chat {
        Some(resolved) => {
            report.ok("模型配置", format!("chat -> {}", resolved.model));
        }
        None => {
            report.err("模型配置", "未配置 chat 路由");
            report.blank();
            report.hint("可执行：");
            report.hint("  tiangong model add-provider deepseek \\");
            report.hint("    --protocol deepseek \\");
            report.hint("    --base-url https://api.deepseek.com \\");
            report.hint("    --api-key-env DEEPSEEK_API_KEY");
            report.hint("");
            report.hint("  tiangong model add-model deepseek-chat \\");
            report.hint("    --provider deepseek --model-id deepseek-chat --capability chat");
            report.hint("");
            report.hint("  tiangong model route chat deepseek-chat");
            report.blank();
        }
    }

    // 模型连通性（深度诊断）
    if deep {
        if let Some(resolved) = chat {
            let cfg = tiangong_core::model::ModelProviderConfig {
                api_auth_token: resolved.api_key,
                api_base_url: resolved.base_url,
                api_timeout_ms: resolved.timeout_ms.to_string(),
                api_protocol: resolved.protocol,
                api_model: resolved.model.clone(),
                api_lite_model: String::new(),
            };
            match tiangong_core::model::SingleProviderClient::list_models(&cfg) {
                Ok(_) => report.ok("模型连通性", format!("{} 请求成功", resolved.model)),
                Err(e) => report.err("模型连通性", format!("{e:#}")),
            }
        } else {
            report.skip("模型连通性", "chat 路由未配置，跳过");
        }
    } else {
        report.skip("模型连通性", "使用 --deep 启用");
    }
}

fn check_server(report: &mut DoctorReport) {
    let server = load_server_config();
    report.ok("Server 配置", format!("{}:{}", server.host, server.port));
}

fn check_server_token(report: &mut DoctorReport) {
    let server = load_server_config();
    match &server.auth_token {
        Some(t) if !t.trim().is_empty() => {
            report.ok("Server Token", "已配置");
        }
        _ => {
            report.warn("Server Token", "未配置（接口将无鉴权）");
            report.hint("可执行：tiangong server token generate");
        }
    }
}

fn check_memory(report: &mut DoctorReport) {
    let memory = MemoryConfig::load_or_default();
    let disabled = is_memory_disabled();
    let has_model = memory
        .model
        .as_ref()
        .map(|m| {
            !m.base_url.trim().is_empty()
                && !m.api_key.trim().is_empty()
                && !m.model.trim().is_empty()
        })
        .unwrap_or(false);

    if disabled {
        report.warn("Memory 配置", "已禁用");
    } else if has_model {
        let model_name = memory
            .model
            .as_ref()
            .map(|m| m.model.clone())
            .unwrap_or_default();
        report.ok("Memory 配置", format!("已启用，LLM={model_name}"));
    } else {
        report.warn("Memory 配置", "已启用但 LLM 端点未配置");
        report.hint("可执行：tiangong memory config set --llm <模型名>");
    }
}

fn check_mcp(report: &mut DoctorReport) {
    let state = tiangong_core::app_state::TiangongState::load_or_default();
    let count = state.agent_config().mcp.servers.len();
    if count == 0 {
        report.warn("MCP 配置", "0 个服务");
    } else {
        report.ok("MCP 配置", format!("{count} 个服务可解析"));
    }
}

fn check_skills(report: &mut DoctorReport) {
    let state = tiangong_core::app_state::TiangongState::load_or_default();
    let skills = state.installed_skills();
    if skills.is_empty() {
        report.warn("Skill 目录", "0 个 Skill");
    } else {
        report.ok("Skill 目录", format!("{} 个 Skill 可用", skills.len()));
    }
}

fn check_custom_prompt(report: &mut DoctorReport) {
    let path = custom_prompt_path();
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let chars = content.trim().chars().count();
            if chars == 0 {
                report.warn("自定义 Prompt", "文件存在但为空");
            } else {
                report.ok("自定义 Prompt", format!("已配置，{chars} 字"));
            }
        } else {
            report.err("自定义 Prompt", format!("读取失败：{}", path.display()));
        }
    } else {
        report.skip("自定义 Prompt", "未配置（可选）");
    }
}
