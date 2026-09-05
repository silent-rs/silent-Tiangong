//! 工具规格与系统提示注入（与 protocol::ops::TOOL_OPERATIONS 一一对应）。

pub const PROMPT_SECTION: &str = "Subagent 工具使用规范：需要帮手时先用 create_agent 招募（同名成员会复用并延续其长期指令与记忆；默认创建后立即在当前会话激活，随后可直接派活）。招募通常选原生后端 agent_team（无需任何会话或命令参数，系统为其建立专属天工运行时会话并跨任务延续上下文），仅在需要关联某个已有会话（tiangong_session，需 session_id 或 session_query）或接入外部命令（cli，需 command）时选用其他后端。list_agents 查看全部持久 Subagent；向某个 Subagent 交办工作前必须已在当前会话激活（自动绑定当前 Workspace）。用户消息以「@成员名」开头或包含 @ 提及时，表示希望把内容定向交给该 Subagent：把去除 @ 标记后的内容用 send_agent_message 转达给对应成员（必要时先 activate_agent）。追问、补充背景、纠正方向用 send_agent_message；有明确目标和完成条件的正式工作用 submit_agent_task（立即返回，完成、阻塞、审批或失败会自动反馈到本会话，无需轮询等待）。跟踪进度用 list_agent_events / get_agent_run / get_agent_task；需要停止时用 interrupt_agent_run（可恢复现场）或 cancel_agent_run（终态）。读取与积累 Subagent 的长期记忆用 get_agent_memory / append_agent_memory。同一 Workspace 同时只有一个写入者，激活被拒时说明工作区被占用。成员之间可组成集群协同作业：执行中需要同伴（其他 Subagent 成员）协助、提供信息或接续工作时，用 send_agent_message 向该成员发送协作消息（对方完成后的回复会自动送回本会话）；收到「【Subagent 消息】来自成员「XX」」即同伴的协作请求，处理后在最终回复中给出结果即可，也可继续用 send_agent_message 与更多成员协作。Subagent 具备成长进化能力：完成一项工作后，把可复用经验（成功做法、踩坑、用户偏好，一行一条结论式）用 append_agent_memory（memory_name 指定 lessons.md）沉淀为经验记忆（后续运行优先注入）；确有稳定下来的新规则时，用 append_agent_instructions 追加进自己的长期指令——只记结论，不记流水。";

/// (工具名, 描述, input_schema JSON)。
pub const TOOL_SPECS: &[(&str, &str, &str)] = &[
    (
        "create_agent",
        "招募一个 Subagent 协作成员：创建持久身份（或复用同名成员并延续其指令与记忆），默认创建后立即在当前会话激活，之后即可用 send_agent_message / submit_agent_task 派活。后端三选一：agent_team（推荐默认，无需额外参数，系统为其建立专属天工运行时会话并跨任务延续上下文）、cli（提供 command，子进程走 JSONL 协议）或 tiangong_session（提供 session_id 或 session_query 按标题关联一个已有会话，复用其上下文）。",
        r#"{"type":"object","properties":{"name":{"type":"string","description":"成员名称；同名已存在时直接复用"},"description":{"type":"string","description":"职责一句话说明"},"backend":{"type":"string","enum":["agent_team","cli","tiangong_session"],"description":"运行后端，推荐默认 agent_team（原生 Subagent，无需额外参数）"},"command":{"type":"string","description":"cli 后端的启动命令（stdin/stdout 走 JSONL 协议）"},"session_id":{"type":"string","description":"tiangong_session：直接指定关联会话 ID"},"session_query":{"type":"string","description":"tiangong_session：按标题关键词搜索会话并取最近匹配"},"workspace_policy":{"type":"string","enum":["read-only","read-write-exclusive","isolated-worktree"],"description":"Workspace 策略，默认只读"},"instructions":{"type":"string","description":"长期指令（该成员的职责与工作要求，跨会话保留）"},"activate":{"type":"boolean","description":"创建后是否立即在当前会话激活，默认 true"}},"required":["name"]}"#,
    ),
    (
        "list_agents",
        "查看全部持久 Subagent 的列表、后端类型、当前会话激活状态与运行实例状态。",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "get_agent",
        "查看某个 Subagent 的详情：身份配置、长期指令、最近任务与事件、产物数量。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"Agent ID（list_agents 结果中给出）"}},"required":["agent_id"]}"#,
    ),
    (
        "activate_agent",
        "在当前会话激活某个 Subagent，绑定当前会话 Workspace（只读/独占写/隔离 worktree 策略生效）。激活后才能发送消息与提交任务。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"要激活的 Agent ID"}},"required":["agent_id"]}"#,
    ),
    (
        "deactivate_agent",
        "在当前会话停用某个 Subagent：中断其在本会话上的运行实例并释放 Workspace 写入所有权。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"要停用的 Agent ID"}},"required":["agent_id"]}"#,
    ),
    (
        "list_active_agents",
        "查看当前会话已激活的 Subagent 及其运行实例状态。",
        r#"{"type":"object","properties":{}}"#,
    ),
    (
        "send_agent_message",
        "向指定 Subagent 发送普通消息：用于追问、补充背景、纠正方向和一般交流。有运行实例时注入当前运行，否则启动一次轻量消息往返；回复经反馈通道返回发起会话。Subagent 成员之间也可用它互相发送协作消息（agent_id 支持成员 ID 或名称，目标成员无需在发起会话激活，对方完成后回复自动送回发起会话）。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"目标成员的 Agent ID 或名称（主会话发起时须已在当前会话激活；成员间协作无需激活）"},"content":{"type":"string","description":"消息内容"}},"required":["agent_id","content"]}"#,
    ),
    (
        "submit_agent_task",
        "向指定 Subagent 提交有明确完成条件的正式任务。立即返回任务与运行编号，执行在后台进行；完成、阻塞、审批或失败会反馈到本会话。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"目标 Agent ID（须已在当前会话激活）"},"goal":{"type":"string","description":"任务目标（要做什么）"},"completion_criteria":{"type":"string","description":"完成条件（怎样算做完）"}},"required":["agent_id","goal"]}"#,
    ),
    (
        "get_agent_task",
        "查看某个任务的详情与全部运行记录。",
        r#"{"type":"object","properties":{"task_id":{"type":"string","description":"任务 ID（submit_agent_task 结果中给出）"}},"required":["task_id"]}"#,
    ),
    (
        "list_agent_tasks",
        "查看任务列表（默认当前会话，指定 agent_id 时列出该 Agent 全部任务）。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"可选，按 Agent 过滤"}}}"#,
    ),
    (
        "get_agent_run",
        "查看某次运行的详情与事件流（输出摘要、终态、时间线）。",
        r#"{"type":"object","properties":{"run_id":{"type":"string","description":"运行 ID"}},"required":["run_id"]}"#,
    ),
    (
        "interrupt_agent_run",
        "中断某个运行（保留现场，运行可继续输出或被再次控制）。",
        r#"{"type":"object","properties":{"run_id":{"type":"string","description":"要中断的运行 ID"}},"required":["run_id"]}"#,
    ),
    (
        "cancel_agent_run",
        "取消某个运行（终态，托管进程会被终止）。",
        r#"{"type":"object","properties":{"run_id":{"type":"string","description":"要取消的运行 ID"}},"required":["run_id"]}"#,
    ),
    (
        "list_agent_events",
        "查看事件历史（运行状态、输出、阻塞、审批、完成、失败），可按 Agent 或运行过滤。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"可选，按 Agent 过滤"},"run_id":{"type":"string","description":"可选，按运行过滤"},"limit":{"type":"integer","description":"返回条数上限，默认 20","minimum":1}}}"#,
    ),
    (
        "get_agent_artifacts",
        "查看某个 Subagent 的历史产物列表（身份目录 artifacts/ 下）。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"Agent ID"}},"required":["agent_id"]}"#,
    ),
    (
        "get_agent_memory",
        "读取某个 Subagent 的长期记忆（memory/ 下全部文件的注入形态摘要），供了解其积累的经验与偏好。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"Agent ID"}},"required":["agent_id"]}"#,
    ),
    (
        "append_agent_memory",
        "向某个 Subagent 的长期记忆追加一条结论（如用户偏好、项目约定、任务经验），后续运行会自动携带。可复用经验建议指定 memory_name 为 lessons.md（注入时优先供给）。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"Agent ID"},"content":{"type":"string","description":"要记住的结论内容"},"note":{"type":"string","description":"可选备注（来源或场景）"},"memory_name":{"type":"string","description":"目标记忆文件名，默认 notes.md；可复用经验用 lessons.md"}},"required":["agent_id","content"]}"#,
    ),
    (
        "append_agent_instructions",
        "向某个 Subagent 的长期指令追加一条稳定下来的新规则（职责要求、工作方式、用户长期偏好；带日期分段追加，不覆盖既有内容）。成员只能追加自己的指令，主会话可操作任意成员。临时性内容请改用 append_agent_memory。",
        r#"{"type":"object","properties":{"agent_id":{"type":"string","description":"Agent ID（成员身份发起时只能是自己）"},"addition":{"type":"string","description":"要追加的规则（简洁、可长期遵循，勿与既有指令重复）"}},"required":["agent_id","addition"]}"#,
    ),
];
