//! Coding 工作模式 WASM 插件。

mod bindings;
mod sidecar_client;

use std::cell::RefCell;

use bindings::exports::tiangong::plugin::plugin::{
    Guest, PluginDescriptor, PluginError, ToolCall, ToolResult, ToolSpec,
};
use bindings::exports::tiangong::plugin::plugin_ui::{
    Contribution, Guest as UiGuest, ResourceResponse, ViewMessageRequest, ViewMessageResponse,
    ViewResponse,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tiangong_plugin_coding_protocol::{
    Checkpoint, CheckpointRequest, Preflight, PreflightRequest, ProjectContext,
    ProjectContextResponse, Review, ReviewRequest, TOOL_CHECKPOINT, TOOL_PREFLIGHT,
    TOOL_PROJECT_CONTEXT, TOOL_REVIEW, VerificationResult, WorkspaceRequest,
};

mod descriptor {
    pub const ID: &str = tiangong_plugin_coding_protocol::PLUGIN_ID;
    pub const NAME: &str = "Coding";
    pub const VERSION: &str = tiangong_plugin_coding_protocol::PLUGIN_VERSION;
}

#[derive(Default)]
struct Context {
    workspace: Option<String>,
    full_trust: bool,
}

thread_local! {
    static CONTEXT: RefCell<Context> = RefCell::new(Context::default());
}

fn plugin_err(message: impl Into<String>) -> PluginError {
    PluginError::Message(message.into())
}

struct Component;

impl Guest for Component {
    fn describe() -> Result<PluginDescriptor, PluginError> {
        Ok(PluginDescriptor {
            id: descriptor::ID.to_string(),
            name: descriptor::NAME.to_string(),
            version: descriptor::VERSION.to_string(),
        })
    }

    fn tool_specs() -> Result<Vec<ToolSpec>, PluginError> {
        Ok(vec![
            ToolSpec {
                name: TOOL_PROJECT_CONTEXT.to_string(),
                description: "发现当前工作区的项目类型、规则与工作流文件、版本控制状态及可用检查命令。进入不熟悉的项目或需要确定验证方式时使用；此工具只读取和分析，不修改项目。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "当前任务；提供后只恢复与该任务匹配的进度"
                        }
                    }
                })
                .to_string(),
            },
            ToolSpec {
                name: TOOL_PREFLIGHT.to_string(),
                description: "在开始实际修改前核对任务说明、项目约定、工作区状态和完成标准。只返回阻碍与提醒，不创建分支、不修改文件。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "本次需要完成的开发任务"
                        }
                    },
                    "required": ["task"]
                })
                .to_string(),
            },
            ToolSpec {
                name: TOOL_CHECKPOINT.to_string(),
                description: "记录长任务的完成标准、进展、改动和真实验证结果，供后续轮次恢复。记录保存在插件私有目录，不写入项目仓库。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "当前任务" },
                        "completion_criteria": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "可验证的完成标准"
                        },
                        "completed": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "已完成事项"
                        },
                        "changed_files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "本任务已修改文件"
                        },
                        "verification": verification_schema(),
                        "blockers": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "当前外部阻碍"
                        }
                    },
                    "required": ["task"]
                })
                .to_string(),
            },
            ToolSpec {
                name: TOOL_REVIEW.to_string(),
                description: "交付前核对分支与工作区的实际改动范围和验证结果。自动选择上游基线；这只是交付范围检查，不替代代码质量审查，也不修改、提交或清理改动。未通过时必须继续处理并重新审查，不能直接结束任务。".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "base_ref": {
                            "type": "string",
                            "description": "可选 Git 基线引用；默认自动选择上游、远端默认分支或本地主分支"
                        },
                        "allowed_paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "可选的少量允许文件或目录前缀；为空时只报告实际范围，不判断越界"
                        },
                        "verification": verification_schema()
                    }
                })
                .to_string(),
            },
        ])
    }

    fn prompt_sections() -> Result<Vec<String>, PluginError> {
        Ok(vec![coding_prompt()])
    }

    fn handle_tool(call: ToolCall) -> Result<ToolResult, PluginError> {
        match call.name.as_str() {
            TOOL_PROJECT_CONTEXT => handle_project_context(&call),
            TOOL_PREFLIGHT => handle_preflight(&call),
            TOOL_CHECKPOINT => handle_checkpoint(&call),
            TOOL_REVIEW => handle_review(&call),
            other => Err(plugin_err(format!("未知的 Coding 工具: {other}"))),
        }
    }

    fn shutdown() -> Result<(), PluginError> {
        Ok(())
    }

    fn set_workspace(workspace: Option<String>, full_trust: bool) -> Result<(), PluginError> {
        CONTEXT.with(|value| {
            let mut value = value.borrow_mut();
            value.workspace = workspace;
            value.full_trust = full_trust;
        });
        Ok(())
    }

    fn on_config_updated(_config_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ready(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_started(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_turn_finished(_session_json: String, _turn_start_idx: u32) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_session_ended(_session_json: String) -> Result<(), PluginError> {
        Ok(())
    }
}

impl UiGuest for Component {
    fn contributions() -> Result<Vec<Contribution>, PluginError> {
        Ok(Vec::new())
    }

    fn open_view(_id: String) -> Result<ViewResponse, PluginError> {
        Err(plugin_err("Coding 插件暂无设置页面"))
    }

    fn get_view_resource(_path: String) -> Result<ResourceResponse, PluginError> {
        Err(plugin_err("Coding 插件暂无页面资源"))
    }

    fn handle_view_message(
        _request: ViewMessageRequest,
    ) -> Result<ViewMessageResponse, PluginError> {
        Err(plugin_err("Coding 插件暂无页面消息"))
    }
}

#[derive(Deserialize)]
struct PreflightArgs {
    task: String,
}

#[derive(Default, Deserialize)]
struct ProjectContextArgs {
    #[serde(default)]
    task: Option<String>,
}

#[derive(Deserialize)]
struct CheckpointArgs {
    task: String,
    #[serde(default)]
    completion_criteria: Vec<String>,
    #[serde(default)]
    completed: Vec<String>,
    #[serde(default)]
    changed_files: Vec<String>,
    #[serde(default)]
    verification: Vec<VerificationResult>,
    #[serde(default)]
    blockers: Vec<String>,
}

#[derive(Default, Deserialize)]
struct ReviewArgs {
    #[serde(default)]
    base_ref: Option<String>,
    #[serde(default)]
    allowed_paths: Vec<String>,
    #[serde(default)]
    verification: Vec<VerificationResult>,
}

fn handle_project_context(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: ProjectContextArgs = parse_arguments(call)?;
    let (workspace, full_trust) = current_context()?;
    let response: ProjectContextResponse =
        sidecar_client::invoke::<ProjectContext>(&WorkspaceRequest {
            workspace,
            full_trust,
            task: args.task,
        })
        .map_err(|error| plugin_err(format!("获取项目上下文失败: {error}")))?;
    json_result(true, "已发现当前项目开发上下文", &response, Vec::new())
}

fn handle_preflight(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: PreflightArgs = parse_arguments(call)?;
    let (workspace, full_trust) = current_context()?;
    let response = sidecar_client::invoke::<Preflight>(&PreflightRequest {
        workspace,
        full_trust,
        task: args.task,
    })
    .map_err(|error| plugin_err(format!("开发前检查失败: {error}")))?;
    let ok = response.blockers.is_empty();
    let summary = if ok {
        "开发前检查完成"
    } else {
        "开发前检查发现阻碍"
    };
    json_result(ok, summary, &response, response.blockers.clone())
}

fn handle_checkpoint(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: CheckpointArgs = parse_arguments(call)?;
    let (workspace, _) = current_context()?;
    let response = sidecar_client::invoke::<Checkpoint>(&CheckpointRequest {
        workspace,
        task: args.task,
        completion_criteria: args.completion_criteria,
        completed: args.completed,
        changed_files: args.changed_files,
        verification: args.verification,
        blockers: args.blockers,
    })
    .map_err(|error| plugin_err(format!("记录开发进度失败: {error}")))?;
    json_result(true, "开发进度已记录", &response, Vec::new())
}

fn handle_review(call: &ToolCall) -> Result<ToolResult, PluginError> {
    let args: ReviewArgs = parse_arguments(call)?;
    let (workspace, _) = current_context()?;
    let response = sidecar_client::invoke::<Review>(&ReviewRequest {
        workspace,
        base_ref: args.base_ref,
        allowed_paths: args.allowed_paths,
        verification: args.verification,
    })
    .map_err(|error| plugin_err(format!("交付审查失败: {error}")))?;
    let summary = if response.ready {
        "交付审查通过"
    } else {
        "交付审查仍有未完成项；继续处理并重新审查，不要结束当前任务"
    };
    json_result(response.ready, summary, &response, response.notes.clone())
}

fn parse_arguments<T: DeserializeOwned>(call: &ToolCall) -> Result<T, PluginError> {
    parse_arguments_raw(&call.name, &call.arguments)
}

fn parse_arguments_raw<T: DeserializeOwned>(name: &str, arguments: &str) -> Result<T, PluginError> {
    serde_json::from_str(arguments).map_err(|error| plugin_err(format!("{name} 参数无效: {error}")))
}

fn current_context() -> Result<(String, bool), PluginError> {
    CONTEXT.with(|value| {
        let value = value.borrow();
        value
            .workspace
            .clone()
            .filter(|workspace| !workspace.trim().is_empty())
            .map(|workspace| (workspace, value.full_trust))
            .ok_or_else(|| plugin_err("当前会话未设置工作区"))
    })
}

fn json_result<T: serde::Serialize>(
    ok: bool,
    summary: &str,
    value: &T,
    errors: Vec<String>,
) -> Result<ToolResult, PluginError> {
    let stdout = serde_json::to_string_pretty(value)
        .map_err(|error| plugin_err(format!("序列化 Coding 工具结果失败: {error}")))?;
    Ok(ToolResult {
        ok,
        summary: summary.to_string(),
        stdout,
        stderr: errors.join("\n"),
        exit_code: if ok { 0 } else { 1 },
        execution: None,
    })
}

fn verification_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "实际执行的检查或验证" },
                "passed": { "type": "boolean", "description": "是否通过" },
                "details": { "type": "string", "description": "关键结果或失败原因" }
            },
            "required": ["name", "passed"]
        },
        "description": "已实际执行的验证结果；未运行时保持为空"
    })
}

fn coding_prompt() -> String {
    let workspace = CONTEXT.with(|value| value.borrow().workspace.clone());
    let full_trust = CONTEXT.with(|value| value.borrow().full_trust);
    let mut prompt = String::from(CODING_WORKFLOW);
    prompt.push_str("\n\n当前工作区：");
    prompt.push_str(workspace.as_deref().unwrap_or("未设置"));
    prompt.push_str("；信任模式：");
    prompt.push_str(if full_trust { "完全信任" } else { "受限" });
    prompt
}

const CODING_WORKFLOW: &str = r#"## Coding 工作模式

处理需要修改代码、配置或工程文件的任务时，遵循以下通用流程：

1. 开始前明确可验证的完成标准。先发现并遵循当前项目已有的规则、需求、规划、任务管理和贡献约定，不假设它们使用固定文件名或固定流程；只有项目确实采用相关记录时才核对和维护。
2. 面对不熟悉的项目，先携带当前任务调用 `coding_project_context` 获取结构化上下文，再读取其中与当前任务有关的规则和项目文件。实际修改前调用 `coding_preflight` 核对任务边界与工作区风险。
3. 先使用可用的检索能力缩小范围，再精确搜索并按需读取文件；根据真实代码和配置作出判断，不猜测项目结构、依赖或行为。
4. 只做满足需求所需的最小改动，复用项目已有模式。不要覆盖、清理或混入用户原有工作，也不要未经授权执行提交、推送、合并或破坏性操作。
5. 根据项目自己的清单、脚本、持续集成配置和规则选择最小充分验证，不预设语言、框架、包管理器或命令。所有命令使用合理时限；需要持续交互时才使用终端能力。
6. 验证失败时读取真实输出，修复后重新运行。长任务可用 `coding_checkpoint` 记录进度；交付前用 `coding_review` 自动核对分支与工作区改动范围和实际验证结果。该检查不能替代代码质量审查；未通过时继续修复并重新审查。
7. 不以单轮回复或阶段总结为任务边界。进入完成度判断或总结阶段时，只要仍有能够自行执行的工作，首行必须准确输出 `[NEED_MORE_WORK]`，让系统继续执行；不要向用户交付阶段性未完成报告，也不要要求用户发送“继续”。
8. 未完成、编译或验证失败、审查未通过、待同步记录、工作量较大、已执行多轮、时间或上下文消耗都不是外部阻碍，必须继续处理。只有缺少必要的用户决定、授权、凭据，或无法替代的外部服务、硬件和资源时才可暂停并说明具体缺口。
9. 只有需求全部完成且相关验证通过后才结束，并用简单直白的语言说明完成内容和结果。

通用文件、检索、命令和终端能力继续由现有工具提供；Coding 插件只补充开发工作流、项目上下文、进度记录和交付审查，不重复实现这些原子能力。"#;
bindings::export!(Component with_types_in bindings);
