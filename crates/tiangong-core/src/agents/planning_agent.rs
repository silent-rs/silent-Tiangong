use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::agent_config::AgentConfig;
use crate::mcp::build_mcp_hints;
use crate::model::{ModelClient, ModelRequest, ModelStreamChunk, TokenUsage};
use crate::planner::{PlanItem, PlanStep, PlanStepSource, PlanStepStatus, TaskPlan};
use crate::session::{MessageRole, Session};
use crate::skill::build_skill_hints;

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct PlanningLlmOutput {
    pub content: String,
    pub reasoning_content: String,
    pub usage: TokenUsage,
}

pub fn build_minimal_plan(user_input: &str, agent_config: &AgentConfig) -> TaskPlan {
    let has_tool_intent =
        user_input.contains("读取") || user_input.contains("文件") || user_input.contains("目录");
    let has_command_intent = user_input.contains("命令");

    let step = if user_input.contains("读取") || user_input.contains("文件") {
        PlanStep {
            id: new_id(),
            name: "prepare_tool_call".to_string(),
            description: "准备工具调用参数并进入执行阶段".to_string(),
            status: PlanStepStatus::Pending,
            source: PlanStepSource::Planned,
        }
    } else {
        PlanStep {
            id: new_id(),
            name: "generate_response".to_string(),
            description: "基于上下文生成回答".to_string(),
            status: PlanStepStatus::Pending,
            source: PlanStepSource::Planned,
        }
    };

    let objective = if has_tool_intent || has_command_intent {
        "通过最小工具链完成信息收集后生成回答".to_string()
    } else {
        "基于上下文直接生成高质量回答".to_string()
    };

    let risks = if has_command_intent {
        vec![
            "命令执行可能失败或被权限策略拦截".to_string(),
            "命令输出可能被截断，需要二次确认".to_string(),
        ]
    } else if has_tool_intent {
        vec![
            "目标路径可能不存在或越界".to_string(),
            "读取内容可能过长导致摘要丢失细节".to_string(),
        ]
    } else {
        vec!["上下文信息不足时回答可能不完整".to_string()]
    };

    let skill_hints = build_skill_hints(user_input, &agent_config.skills);
    let mcp_hints = build_mcp_hints(user_input, &agent_config.mcp);

    let mut plan = TaskPlan {
        id: new_id(),
        objective,
        summary: format!("针对输入“{}”生成最小执行计划", user_input),
        plans: Vec::new(),
        risks,
        skill_hints,
        mcp_hints,
        revisions: Vec::new(),
    };
    plan.push_plan(
        "execute_core_task",
        "执行当前任务并给出可交付结果",
        vec![step],
    );
    plan
}

#[allow(dead_code)]
pub fn build_plan_with_agent(
    client: &impl ModelClient,
    session: &Session,
    user_input: &str,
    agent_config: &AgentConfig,
    context_limit: usize,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> TaskPlan {
    build_plan_with_agent_with_trace(
        client,
        session,
        user_input,
        agent_config,
        context_limit,
        on_delta,
    )
    .0
}

pub fn build_plan_with_agent_with_trace(
    client: &impl ModelClient,
    session: &Session,
    user_input: &str,
    agent_config: &AgentConfig,
    context_limit: usize,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> (TaskPlan, PlanningLlmOutput) {
    match build_plan_with_agent_inner(
        client,
        session,
        user_input,
        agent_config,
        context_limit,
        on_delta,
    ) {
        Ok((plan, llm_output)) => (plan, llm_output),
        Err(err) => {
            let mut fallback = build_minimal_plan(user_input, agent_config);
            fallback.revise(
                "planning_agent",
                format!("planing 智能体生成失败：{err}"),
                "planing 智能体不可用，已回退为最小计划".to_string(),
            );
            fallback.ensure_risk("planing 智能体不可用，计划细节可能不足".to_string());
            let llm_output = PlanningLlmOutput {
                content: format!("planning-agent 调用失败，已回退最小计划：{err}"),
                reasoning_content: String::new(),
                usage: TokenUsage::default(),
            };
            (fallback, llm_output)
        }
    }
}

fn build_plan_with_agent_inner(
    client: &impl ModelClient,
    session: &Session,
    user_input: &str,
    agent_config: &AgentConfig,
    context_limit: usize,
    on_delta: &mut dyn FnMut(&ModelStreamChunk),
) -> Result<(TaskPlan, PlanningLlmOutput)> {
    let input_list = collect_user_input_list(session, user_input, context_limit);
    let skill_hints = build_skill_hints(user_input, &agent_config.skills);
    let mcp_hints = build_mcp_hints(user_input, &agent_config.mcp);
    let request = ModelRequest {
        session_title: format!("{} · planing-agent", session.title),
        user_input: build_planing_prompt(user_input, &input_list, &skill_hints, &mcp_hints),
        context: session.recent_messages(context_limit),
            assembled_system_prompt: None,
    };
    let response = client
        .complete_stream(&request, on_delta)
        .context("planing 智能体调用失败")?;
    let parsed = parse_planing_output(&response.text).with_context(|| {
        let preview = response.text.chars().take(160).collect::<String>();
        format!("解析 planing 智能体返回失败，返回片段：{}", preview)
    })?;

    let objective = parsed.objective.trim().to_string();
    if objective.is_empty() {
        return Err(anyhow!("planing 输出缺少 objective"));
    }
    let summary = parsed.summary.trim().to_string();
    if summary.is_empty() {
        return Err(anyhow!("planing 输出缺少 summary"));
    }

    let resolved_plans = parsed.resolve_plans();
    let mut plans = Vec::new();
    for (idx, plan_item) in resolved_plans.into_iter().enumerate() {
        let plan_name = normalize_text(plan_item.name);
        let plan_desc = normalize_text(plan_item.description);
        if plan_name.is_empty() && plan_desc.is_empty() {
            continue;
        }

        let mut execution_steps = Vec::new();
        for (step_idx, step) in plan_item.execution_steps.into_iter().enumerate() {
            let name = normalize_text(step.name);
            let description = normalize_text(step.description);
            if description.is_empty() {
                continue;
            }
            execution_steps.push(PlanStep {
                id: new_id(),
                name: if name.is_empty() {
                    format!("step_{}_{}", idx + 1, step_idx + 1)
                } else {
                    name
                },
                description,
                status: PlanStepStatus::Pending,
                source: PlanStepSource::Planned,
            });
        }

        if execution_steps.is_empty() {
            execution_steps.push(PlanStep {
                id: new_id(),
                name: format!("step_{}_1", idx + 1),
                description: if plan_desc.is_empty() {
                    "根据该事项执行并产出结果".to_string()
                } else {
                    plan_desc.clone()
                },
                status: PlanStepStatus::Pending,
                source: PlanStepSource::Planned,
            });
        }
        if execution_steps.len() > 1 {
            execution_steps.truncate(1);
        }

        let mut item = PlanItem {
            id: new_id(),
            name: if plan_name.is_empty() {
                format!("plan_{}", idx + 1)
            } else {
                plan_name
            },
            description: if plan_desc.is_empty() {
                "执行该事项并达成阶段目标".to_string()
            } else {
                plan_desc
            },
            status: PlanStepStatus::Pending,
            execution_summary: None,
            execution_steps,
        };
        item.refresh_status();
        plans.push(item);
    }
    if plans.is_empty() {
        return Err(anyhow!("planing 输出缺少有效 plans"));
    }

    let risks = parsed
        .risks
        .into_iter()
        .map(normalize_text)
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    let risks = if risks.is_empty() {
        vec!["规划信息不足，执行中可能需要动态补充步骤".to_string()]
    } else {
        risks
    };

    Ok((
        TaskPlan {
            id: new_id(),
            objective,
            summary,
            plans,
            risks,
            skill_hints,
            mcp_hints,
            revisions: Vec::new(),
        },
        PlanningLlmOutput {
            content: response.text,
            reasoning_content: response.reasoning_content,
            usage: response.usage,
        },
    ))
}

#[derive(Debug, Deserialize, Clone)]
struct PlaningAgentOutput {
    objective: String,
    summary: String,
    #[serde(default)]
    plans: Vec<PlaningAgentOutputPlan>,
    #[serde(default)]
    steps: Vec<PlaningAgentOutputStep>,
    #[serde(default)]
    risks: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct PlaningAgentOutputPlan {
    name: String,
    description: String,
    #[serde(default)]
    execution_steps: Vec<PlaningAgentOutputStep>,
    #[serde(default)]
    steps: Vec<PlaningAgentOutputStep>,
}

#[derive(Debug, Deserialize, Clone)]
struct PlaningAgentOutputStep {
    name: String,
    description: String,
}

struct NormalizedPlaningOutputPlan {
    name: String,
    description: String,
    execution_steps: Vec<PlaningAgentOutputStep>,
}

impl PlaningAgentOutput {
    fn resolve_plans(&self) -> Vec<NormalizedPlaningOutputPlan> {
        if self.plans.is_empty() {
            return vec![NormalizedPlaningOutputPlan {
                name: "execute_core_task".to_string(),
                description: "执行当前任务并给出结果".to_string(),
                execution_steps: self.steps.clone(),
            }];
        }

        self.plans
            .clone()
            .into_iter()
            .map(|item| {
                let execution_steps = if item.execution_steps.is_empty() {
                    item.steps
                } else {
                    item.execution_steps
                };
                NormalizedPlaningOutputPlan {
                    name: item.name,
                    description: item.description,
                    execution_steps,
                }
            })
            .collect()
    }
}

fn build_planing_prompt(
    user_input: &str,
    input_list: &[String],
    skill_hints: &[String],
    mcp_hints: &[String],
) -> String {
    let input_list_text = if input_list.is_empty() {
        format!("1. {}", user_input.trim())
    } else {
        input_list
            .iter()
            .enumerate()
            .map(|(idx, input)| format!("{}. {}", idx + 1, input))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let skill_hints_text = if skill_hints.is_empty() {
        "无".to_string()
    } else {
        skill_hints.join("；")
    };
    let mcp_hints_text = if mcp_hints.is_empty() {
        "无".to_string()
    } else {
        mcp_hints.join("；")
    };
    format!(
        r#"你是天工内置的 planing 智能体。你的任务是只输出 JSON 格式的执行计划。

输出要求：
1. 只输出一个 JSON 对象，不要 markdown，不要代码块，不要额外解释。
2. JSON 结构必须为：
{{
  "objective": "string",
  "summary": "string",
  "plans": [
    {{
      "name": "string",
      "description": "string",
      "execution_steps": [
        {{"name": "string", "description": "string"}}
      ]
    }}
  ],
  "risks": ["string"]
}}
3. plans 至少 1 条，按执行优先级排序。即使是简单问答也要生成 plan，描述如何回答。
4. 每个 plan 的 execution_steps 至少 1 条，按该 plan 的执行顺序排列。
5. plan 只描述事项，不要把验证命令或验证结论写入 plan/steps。
6. 保持计划通用、可执行，避免空泛描述。
7. 你会收到“用户连续输入列表”，需要基于完整列表进行增量式规划：保留仍有效事项、移除已无效事项、补充新增事项，并输出最终有序 plan。
8. 若存在命中 Skills，plan 必须明确“优先按 Skill 指令执行”，并为后续执行阶段保留可直接落地的调用步骤（命令、目录、入参）。
9. 若存在命中 MCP，plan 必须优先描述基于 MCP 能力的执行步骤，并写明调用目标（server/能力），不要退化为目录浏览。
10. 对 Skill/MCP 相关步骤，不要规划 `env` / `printenv` / `set` / `grep` 等环境探测命令；运行时已注入相关环境变量，缺失配置应由业务命令错误回传。
11. 每个 plan 的 execution_steps 只提供“起始步骤”1 条，不要在 planning 阶段穷举后续步骤；后续步骤由 execution 阶段按执行结果动态补充。

用户连续输入列表（按时间顺序）：
{input_list_text}

命中 Skills：
{skill_hints_text}

命中 MCP：
{mcp_hints_text}

本轮新增输入：
{user_input}"#,
        skill_hints_text = skill_hints_text,
        mcp_hints_text = mcp_hints_text
    )
}

fn collect_user_input_list(
    session: &Session,
    user_input: &str,
    context_limit: usize,
) -> Vec<String> {
    let mut items = session
        .recent_messages(context_limit)
        .into_iter()
        .filter(|message| matches!(message.role, MessageRole::User))
        .map(|message| normalize_text(message.content))
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>();

    let current = normalize_text(user_input.to_string());
    if !current.is_empty()
        && items
            .last()
            .is_none_or(|last| !last.eq_ignore_ascii_case(current.as_str()))
    {
        items.push(current);
    }

    // 避免长会话导致 planning 提示词膨胀，仅保留最近输入列表。
    const MAX_INPUT_ITEMS: usize = 10;
    if items.len() > MAX_INPUT_ITEMS {
        items = items[items.len() - MAX_INPUT_ITEMS..].to_vec();
    }
    items
}

fn parse_planing_output(raw: &str) -> Result<PlaningAgentOutput> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(anyhow!("planing 返回为空"));
    }

    for candidate in collect_json_candidates(raw) {
        if let Ok(parsed) = serde_json::from_str::<PlaningAgentOutput>(&candidate) {
            return Ok(parsed);
        }
    }

    Err(anyhow!("未匹配到有效 JSON"))
}

fn collect_json_candidates(raw: &str) -> Vec<String> {
    let mut candidates = vec![raw.to_string()];

    if let Some(stripped) = strip_markdown_code_block(raw) {
        candidates.push(stripped);
    }

    if let Some(extracted) = extract_first_json_object(raw) {
        candidates.push(extracted);
    }

    candidates.retain(|item| !item.trim().is_empty());
    candidates.dedup();
    candidates
}

fn strip_markdown_code_block(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    let mut lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        lines.pop();
    }

    Some(lines.join("\n").trim().to_string())
}

fn extract_first_json_object(raw: &str) -> Option<String> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in raw.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth = depth.saturating_add(1);
            }
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    let begin = start?;
                    return Some(raw[begin..=idx].to_string());
                }
            }
            _ => {}
        }
    }

    None
}

fn normalize_text(input: String) -> String {
    input.trim().to_string()
}

fn new_id() -> String {
    scru128::new().to_string()
}
