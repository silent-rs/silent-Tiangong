//! Prompt 组装集成测试
//!
//! 验证插件注册后 system prompt 的组装顺序：
//! - 多个插件的 PromptSectionProvider 按注册顺序追加
//! - 产品文案插件注册在最前时，身份 / 规则段排在 prompt 开头
//! - 插件段后接环境段（会话标题 / 工作目录 / 文件根）与摘要段
//! - 空白段落被过滤；无插件时仍能构建合法 prompt

use std::sync::Arc;

use tiangong_core::agent_config::AgentConfig;
use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::model::SingleProviderClient;
use tiangong_core::prompt::SystemPromptConfig;
use tiangong_core::prompt::sections::build_full_system_prompt;
use tiangong_core::runtime::RuntimeEngine;
use tiangong_core::session::Session;
use tiangong_core::tool_override::PromptSectionProvider;

// ── 测试用 PromptSectionProvider ─────────────────────────────

/// 模拟产品文案插件：返回固定顺序的多段。
struct ProductCopyProvider;

impl PromptSectionProvider for ProductCopyProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec![
            "【产品身份】你是测试助手".to_string(),
            "【通用规则】使用 Markdown".to_string(),
        ]
    }
}

/// 模拟能力插件：返回单段。
struct CapabilityProvider {
    section: &'static str,
}

impl PromptSectionProvider for CapabilityProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec![self.section.to_string()]
    }
}

/// 返回空白段落的插件，用于验证过滤行为。
struct WhitespaceProvider;

impl PromptSectionProvider for WhitespaceProvider {
    fn prompt_sections(&self) -> Vec<String> {
        vec!["   \n\n  ".to_string(), "有效段".to_string()]
    }
}

// ── 辅助构造 ─────────────────────────────────────────────────

/// 用 dummy 端点构造 RuntimeEngine（不发起真实请求，仅供 prompt 组装测试）。
fn test_engine() -> RuntimeEngine {
    let client = SingleProviderClient::new(ModelEndpoint {
        base_url: "http://127.0.0.1:0/v1".to_string(),
        api_key: "test-key".to_string(),
        model: "test-model".to_string(),
        ..Default::default()
    });
    let storage_root = std::env::temp_dir().join("tiangong-prompt-assembly-test");
    RuntimeEngine::new(client, 8_192, AgentConfig::default(), storage_root)
}

/// 从 engine 收集插件段落并构建 system prompt 文本。
fn build_prompt_text(engine: &RuntimeEngine, session: &Session) -> String {
    let sections = engine.collect_plugin_prompt_sections();
    let config = SystemPromptConfig::from_plugin_sections(sections);
    let msg = build_full_system_prompt(session, &config);
    msg.text_content()
}

// ── 测试用例 ─────────────────────────────────────────────────

#[test]
fn sections_assembled_in_registration_order() {
    let engine = test_engine();
    // 注册顺序：产品文案 → 能力插件 A → 能力插件 B
    engine.register_prompt_section_provider(Arc::new(ProductCopyProvider));
    engine.register_prompt_section_provider(Arc::new(CapabilityProvider {
        section: "【能力A】检索工具",
    }));
    engine.register_prompt_section_provider(Arc::new(CapabilityProvider {
        section: "【能力B】团队协作",
    }));

    let session = Session::new("顺序测试");
    let text = build_prompt_text(&engine, &session);

    // 验证各段出现的索引按注册顺序递增
    let id_idx = text.find("【产品身份】").expect("应包含产品身份段");
    let rules_idx = text.find("【通用规则】").expect("应包含通用规则段");
    let cap_a_idx = text.find("【能力A】").expect("应包含能力A段");
    let cap_b_idx = text.find("【能力B】").expect("应包含能力B段");
    assert!(id_idx < rules_idx, "产品身份应在通用规则前");
    assert!(rules_idx < cap_a_idx, "通用规则应在能力A前");
    assert!(cap_a_idx < cap_b_idx, "能力A应在能力B前");
}

#[test]
fn product_copy_precedes_capability_sections() {
    let engine = test_engine();
    // 产品文案注册在最前，保证身份 / 规则段排在 prompt 开头
    engine.register_prompt_section_provider(Arc::new(ProductCopyProvider));
    engine.register_prompt_section_provider(Arc::new(CapabilityProvider {
        section: "【能力】终端交互",
    }));

    let session = Session::new("首位测试");
    let text = build_prompt_text(&engine, &session);

    // 产品身份段应出现在 prompt 最前面的区域（在环境段之前）
    let id_idx = text.find("【产品身份】").expect("应包含产品身份段");
    let env_idx = text.find("当前会话").expect("应包含环境段");
    assert!(
        id_idx < env_idx,
        "产品身份段应排在环境段之前（保证在 prompt 开头）"
    );
}

#[test]
fn environment_and_summary_sections_appended_after_plugins() {
    let engine = test_engine();
    engine.register_prompt_section_provider(Arc::new(ProductCopyProvider));

    let mut session = Session::new("环境摘要测试");
    // 注入摘要，验证摘要段拼接
    session.context_summary = Some("此前讨论了文件操作".to_string());

    let text = build_prompt_text(&engine, &session);

    // 顺序：插件段 → 环境段 → 摘要段
    let plugin_idx = text.find("【产品身份】").unwrap();
    let env_idx = text.find("当前会话：环境摘要测试").unwrap();
    let summary_idx = text.find("此前对话摘要").unwrap();

    assert!(plugin_idx < env_idx, "插件段应在环境段前");
    assert!(env_idx < summary_idx, "环境段应在摘要段前");
    assert!(text.contains("此前讨论了文件操作"), "应包含摘要内容");
    assert!(text.contains("当前工作目录"), "应包含工作目录");
    assert!(text.contains("允许文件操作目录"), "应包含文件根");
}

#[test]
fn whitespace_only_sections_filtered() {
    let engine = test_engine();
    engine.register_prompt_section_provider(Arc::new(ProductCopyProvider));
    engine.register_prompt_section_provider(Arc::new(WhitespaceProvider));

    let session = Session::new("过滤测试");
    let text = build_prompt_text(&engine, &session);

    // 空白段被过滤，只保留有效段
    assert!(text.contains("【产品身份】"));
    assert!(text.contains("有效段"));
    // 不应出现纯空白形成的多余空行块（有效段不应是孤立的空白）
    assert!(!text.contains("\n\n\n\n"), "不应出现连续多余空行");
}

#[test]
fn empty_plugins_still_produces_valid_prompt() {
    let engine = test_engine();
    // 不注册任何插件

    let session = Session::new("空插件测试");
    let text = build_prompt_text(&engine, &session);

    // 无插件段时，prompt 仍应包含环境段，是合法的非空 system prompt
    assert!(text.contains("当前会话：空插件测试"));
    assert!(text.contains("当前工作目录"));
    assert!(!text.trim().is_empty(), "空插件时 prompt 不应为空");
}

#[test]
fn plugin_sections_appear_between_identity_and_environment() {
    // 综合验证：产品文案（最前）→ 能力插件 → 环境段 → 摘要段
    let engine = test_engine();
    engine.register_prompt_section_provider(Arc::new(ProductCopyProvider));
    engine.register_prompt_section_provider(Arc::new(CapabilityProvider {
        section: "【能力】技能摘要",
    }));
    engine.register_prompt_section_provider(Arc::new(WhitespaceProvider));

    let mut session = Session::new("综合测试");
    session.context_summary = Some("综合摘要内容".to_string());

    let text = build_prompt_text(&engine, &session);

    // 完整顺序断言
    let positions: [usize; 6] = [
        "【产品身份】",
        "【通用规则】",
        "【能力】技能摘要",
        "有效段",
        "当前会话",
        "此前对话摘要",
    ]
    .iter()
    .map(|s| text.find(*s).unwrap_or_else(|| panic!("缺少段落: {s}")))
    .collect::<Vec<_>>()
    .try_into()
    .unwrap();

    for i in 0..positions.len() - 1 {
        assert!(
            positions[i] < positions[i + 1],
            "段落顺序错误：位置 {} 应在 {} 前",
            i,
            i + 1
        );
    }
}
