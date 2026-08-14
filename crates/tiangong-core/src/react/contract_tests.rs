//! 新契约行为测试：工具义务与候选完成门控（requirements.md §5.2）。
//!
//! 本文件按目标行为（ALR-003/006/007/008/307）断言，不固化当前 Summary/
//! ForceFinal 内部阶段。其中大部分用例在当前过渡实现上**预期失败**，以
//! `#[ignore]` 标注启用任务：它们证明"模型漏发必需 tool call 时纯文本被当成
//! 成功"的旧缺陷，并守护任务 15 的 TaskContract 门控不被回退。启用任务完成
//! 后移除 ignore，测试必须转绿。
//!
//! 通过 `cargo test -p tiangong-core -- --ignored` 可单独运行并核对失败形态。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tiangong_llm::{ModelEndpoint, ProviderProtocol};
use tiangong_types::attachment::StoredAsset;
use tiangong_types::{ContentBlock, MediaKind};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::agent_config::AgentConfig;
use crate::model::SingleProviderClient;
use crate::model::{ToolCall, ToolSpec};
use crate::observe::Observer;
use crate::permission::TrustMode;
use crate::prompt::SystemPromptConfig;
use crate::session::Session;
use crate::tool::ToolResult;
use crate::tool_override::{
    MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
};
use crate::turn_context::TurnContext;

use super::execute::execute_turn;
use super::outcome::TurnExecutionOutcome;

/// OpenAI SSE chunk（`data: {json}\n\n`）+ 末尾 `[DONE]`。
fn sse_body(chunks: &[serde_json::Value]) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {}\n\n", chunk));
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

/// 纯文本 delta chunk。
fn text_delta_chunk(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-contract",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": null,
        }],
    })
}

/// tool_calls delta chunk（单个工具调用，一次性给出 name + 完整 arguments）。
fn tool_call_chunk(call_id: &str, name: &str, arguments: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-contract",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                }],
            },
            "finish_reason": "tool_calls",
        }],
    })
}

/// 按挂载顺序（FIFO）挂载一条只响应一次的 SSE mock。
async fn mount_sse(server: &MockServer, chunks: Vec<serde_json::Value>) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_body(&chunks), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// 记录调用并返回固定成功结果的工具覆盖处理器。
struct RecordingTool {
    invocations: Arc<Mutex<Vec<ToolCall>>>,
    ok: bool,
}

impl ToolOverrideHandler for RecordingTool {
    fn handle(
        &self,
        call: &ToolCall,
        _session: &mut Session,
        _actor_id: &str,
    ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
        self.invocations.lock().unwrap().push(call.clone());
        let ok = self.ok;
        Box::pin(async move {
            Some(ToolResult {
                ok,
                summary: if ok {
                    "工具已执行".to_string()
                } else {
                    "工具执行失败".to_string()
                },
                stdout: if ok {
                    "done".to_string()
                } else {
                    String::new()
                },
                stderr: if ok {
                    String::new()
                } else {
                    "failure".to_string()
                },
                exit_code: if ok { 0 } else { 1 },
                execution: None,
            })
        })
    }
}

/// 空实现的占位插件，满足 harness 构造需要。
struct NoopPlugin;

impl ToolOverrideHandler for NoopPlugin {}
impl ToolSpecProvider for NoopPlugin {}
impl PromptSectionProvider for NoopPlugin {}
impl MentionCandidateProvider for NoopPlugin {}
impl crate::core::plugin::Plugin for NoopPlugin {
    fn id(&self) -> &str {
        "contract-noop"
    }
}

fn file_asset() -> StoredAsset {
    StoredAsset {
        asset_id: "asset-contract-1".to_string(),
        local_path: "/tmp/contract-report.pdf".to_string(),
        original_name: "report.pdf".to_string(),
        mime_type: "application/pdf".to_string(),
        size: 1024,
        kind: MediaKind::File,
    }
}

/// 按宿主附件管线的真实形态构造用户消息：
/// `文本 + AssetReference + ModelInstruction`（attachment.rs 的文件附件输出）。
/// 这是入口显式声明的读取义务来源（ALR-006 第一级）。
fn attachment_message_blocks() -> Vec<ContentBlock> {
    let asset = file_asset();
    vec![
        ContentBlock::text("请读取这个附件并总结要点"),
        ContentBlock::asset_reference(asset.clone()),
        ContentBlock::model_instruction(format!(
            "本条用户消息包含文件引用，文件内容不会直接发送给模型。请使用文件工具按下列本地 path 处理。\n- {}",
            asset.original_name
        )),
    ]
}

/// 构造测试 TurnContext：用户消息由 `user_blocks` 指定，注册 read_file 工具。
fn contract_harness(
    server: &MockServer,
    user_blocks: Vec<ContentBlock>,
    tool_ok: bool,
) -> (TurnContext, Arc<Mutex<Vec<ToolCall>>>) {
    let root = tempfile::tempdir().expect("创建临时目录失败");
    let mut session = Session::new("contract-test".to_string());
    session.bind_storage_root(root.path());
    session.append_prepared_user_message_with_id("user-contract-1".to_string(), user_blocks);
    session.rebuild_system_prompt(&SystemPromptConfig::from_plugin_sections(Vec::new()));
    std::mem::forget(root);

    let invocations = Arc::new(Mutex::new(Vec::new()));
    let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
    overrides.insert(
        "read_file".to_string(),
        Arc::new(RecordingTool {
            invocations: invocations.clone(),
            ok: tool_ok,
        }),
    );
    let tools = vec![ToolSpec {
        name: "read_file".to_string(),
        description: "读取文件内容".to_string(),
        input_schema: serde_json::json!({"type": "object", "properties": {}}),
    }];

    let endpoint = ModelEndpoint {
        base_url: server.uri(),
        api_key: "test-key".to_string(),
        model: "test-model".to_string(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        timeout_ms: 5_000,
        options: serde_json::Value::Object(serde_json::Map::new()),
    };
    let (stream_tx, _stream_rx) = std::sync::mpsc::channel::<tiangong_types::StreamEvent>();
    let ctx = TurnContext::builder()
        .client(SingleProviderClient::new(endpoint))
        .session(session)
        .stream_tx(stream_tx)
        .plugins(vec![Arc::new(NoopPlugin)])
        .context_limit(200_000)
        .agent_config(AgentConfig {
            reasoning_effort: "none".to_string(),
            ..Default::default()
        })
        .trust_mode(TrustMode::FullTrust)
        .observer(Observer::new(std::env::temp_dir()))
        .tool_overrides(overrides)
        .tools(tools)
        .build();
    (ctx, invocations)
}

/// 附件义务场景：模型只返回纯文本说明（未调用 read_file），
/// 不应发布成功终态（ALR-003/006/307）。
///
/// 当前实现：ReAct 纯文本 → Summary 完成 → Success（正是要消除的虚假完成）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_request_rejects_text_only_completion() {
    let server = MockServer::start().await;
    // 覆盖当前实现的完整请求序列：ReAct 纯文本 + Summary 完成文本。
    // 新架构下同样挂足修复预算内的纯文本响应（多挂的 mock 无副作用）。
    mount_sse(&server, vec![text_delta_chunk("我读取了附件，内容是……")]).await;
    mount_sse(&server, vec![text_delta_chunk("附件分析完成：要点若干。")]).await;

    let (mut ctx, invocations) = contract_harness(&server, attachment_message_blocks(), true);
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = execute_turn(&mut ctx, &mut cmd_rx).await;

    assert!(
        invocations.lock().unwrap().is_empty(),
        "模型未调用读取工具时，不能把纯文本当作已读取附件的证据"
    );
    assert!(
        !matches!(result.outcome, TurnExecutionOutcome::Success),
        "附件读取义务未满足时，纯文本响应不得发布为成功终态"
    );
}

/// 普通解释类问题：无工具义务时纯文本直接完成，不强制工具（ALR-006 反向约束）。
/// 防过度矫正锚点，当前实现与新架构都必须保持绿色。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_question_without_obligation_completes_without_tool() {
    let server = MockServer::start().await;
    // 无工具义务：单次请求即完成，不再有总结阶段的第二次请求。
    mount_sse(&server, vec![text_delta_chunk("贪心算法是一种……")]).await;

    let (mut ctx, invocations) = contract_harness(
        &server,
        vec![ContentBlock::text("解释一下什么是贪心算法")],
        true,
    );
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = execute_turn(&mut ctx, &mut cmd_rx).await;

    assert!(
        matches!(result.outcome, TurnExecutionOutcome::Success),
        "无工具义务的普通问答应直接成功，当前实现: {:?}",
        result.outcome
    );
    assert!(
        invocations.lock().unwrap().is_empty(),
        "普通问答不应被强制调用工具"
    );
}

/// 附件义务场景：首次漏发工具、第二次修复成功——应执行工具并正常完成（ALR-008）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_obligation_repair_recovers_with_tool_call() {
    let server = MockServer::start().await;
    mount_sse(&server, vec![text_delta_chunk("好的，附件内容是……")]).await;
    mount_sse(
        &server,
        vec![tool_call_chunk(
            "call-repair-1",
            "read_file",
            r#"{"path":"/tmp/contract-report.pdf"}"#,
        )],
    )
    .await;
    mount_sse(&server, vec![text_delta_chunk("附件要点总结如下……")]).await;
    mount_sse(&server, vec![text_delta_chunk("附件要点总结如下……")]).await;

    let (mut ctx, invocations) = contract_harness(&server, attachment_message_blocks(), true);
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = execute_turn(&mut ctx, &mut cmd_rx).await;

    assert_eq!(
        invocations.lock().unwrap().len(),
        1,
        "修复请求后模型返回 tool call，应实际执行 read_file"
    );
    assert!(
        matches!(result.outcome, TurnExecutionOutcome::Success),
        "修复成功后应正常完成，当前实现: {:?}",
        result.outcome
    );
}

/// 附件义务场景：模型持续漏发工具——修复预算耗尽后必须明确失败（ALR-008/009）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_obligation_repair_exhaustion_fails_explicitly() {
    let server = MockServer::start().await;
    // 初始请求 + 修复预算内的所有请求都只返回纯文本。
    for _ in 0..5 {
        mount_sse(&server, vec![text_delta_chunk("我已经读取并分析完了。")]).await;
    }

    let (mut ctx, invocations) = contract_harness(&server, attachment_message_blocks(), true);
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = execute_turn(&mut ctx, &mut cmd_rx).await;

    assert!(
        invocations.lock().unwrap().is_empty(),
        "模型从未真正调用读取工具"
    );
    assert!(
        matches!(result.outcome, TurnExecutionOutcome::Failed(_)),
        "修复耗尽后必须明确失败而非虚假成功，当前实现: {:?}",
        result.outcome
    );
}

/// 工具真实失败不能满足义务：read_file 失败后模型纯文本声称"已完成"，
/// 不应发布成功（ALR-307：只有成功结果可满足要求成功证据的义务）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_tool_result_does_not_satisfy_obligation() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        vec![tool_call_chunk(
            "call-fail-1",
            "read_file",
            r#"{"path":"/tmp/contract-report.pdf"}"#,
        )],
    )
    .await;
    mount_sse(&server, vec![text_delta_chunk("附件内容我已经分析完成。")]).await;
    mount_sse(&server, vec![text_delta_chunk("附件分析完成。")]).await;

    let (mut ctx, invocations) = contract_harness(&server, attachment_message_blocks(), false);
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = execute_turn(&mut ctx, &mut cmd_rx).await;

    assert_eq!(
        invocations.lock().unwrap().len(),
        1,
        "工具应被实际调用过一次"
    );
    assert!(
        !matches!(result.outcome, TurnExecutionOutcome::Success),
        "工具失败的结果不能充当义务证据，不得发布成功终态"
    );
}
