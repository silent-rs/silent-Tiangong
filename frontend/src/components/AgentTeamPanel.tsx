import { useStore } from '@/store/useStore';

/**
 * Agent Team 官方 App（extension.tab / native 容器）：子 Agent 协作状态面板。
 *
 * 数据来自会话实时 token 统计（活跃 agent、各 agent 上下文与累计用量）；
 * 协作编排仍在 Core，本面板只做只读状态展示（设计文档 8.3：UI 与编排
 * 策略走插件，调度核心保留在 Core）。
 */
export function AgentTeamPanel() {
  const tokenStats = useStore((s) => s.tokenStats) ?? {
    active_agent_id: null as string | null,
    agent_token_usage: {} as Record<string, { prompt_tokens: number; completion_tokens: number; total_tokens: number }>,
    agent_current_tokens: {} as Record<string, number>,
  };
  const activeAgentId = tokenStats.active_agent_id;

  const agents = Object.entries(tokenStats.agent_token_usage);
  const currents = tokenStats.agent_current_tokens;

  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="border-b px-4 py-2 text-sm font-medium">Agent Team</div>
      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {agents.length === 0 ? (
          <div className="py-8 text-center text-xs text-muted-foreground">
            当前会话暂无子 Agent 协作。
            <br />
            主 Agent 需要分派任务时会自动创建子 Agent，届时在此查看各成员状态。
          </div>
        ) : (
          <div className="space-y-2">
            {agents.map(([agentId, usage]) => {
              const active = agentId === activeAgentId;
              return (
                <div
                  key={agentId}
                  className={`flex items-center gap-3 rounded-lg border p-3 ${
                    active ? 'border-primary/50 bg-accent' : 'bg-card'
                  }`}
                >
                  <span
                    className={`h-2 w-2 shrink-0 rounded-full ${
                      active ? 'bg-emerald-500' : 'bg-muted-foreground/40'
                    }`}
                    title={active ? '执行中' : '空闲'}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-xs font-medium">{agentId}</div>
                    <div className="text-[11px] text-muted-foreground">
                      上下文 {Math.round((currents[agentId] ?? 0) / 1000)}k / 累计{' '}
                      {Math.round(usage.total_tokens / 1000)}k tokens
                    </div>
                  </div>
                  {active && (
                    <span className="shrink-0 rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] text-emerald-600 dark:text-emerald-400">
                      执行中
                    </span>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
