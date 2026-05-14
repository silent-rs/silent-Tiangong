import { useStore, type AgentInfo } from '@/store/useStore';
import { Users, Circle, ChevronDown, ChevronRight, Square } from 'lucide-react';
import { useEffect, useState } from 'react';

const STATUS_STYLES: Record<string, string> = {
  running: 'bg-green-500',
  idle: 'bg-yellow-500',
  waiting_for_user: 'bg-blue-500',
  waiting_for_lock: 'bg-orange-500',
  terminated: 'bg-muted-foreground',
  error: 'bg-red-500',
};

const STATUS_TEXT_COLORS: Record<string, string> = {
  running: 'text-green-500',
  idle: 'text-yellow-500',
  waiting_for_user: 'text-blue-500',
  waiting_for_lock: 'text-orange-500',
  terminated: 'text-muted-foreground',
  error: 'text-red-500',
};

const STATUS_LABELS: Record<string, string> = {
  running: '运行中',
  idle: '空闲',
  waiting_for_user: '等待用户',
  waiting_for_lock: '等待文件锁',
  terminated: '已结束',
  error: '错误',
};

function StatusDot({ status }: { status: AgentInfo['status'] }) {
  const animate = status === 'running';
  return (
    <Circle
      className={`w-2 h-2 fill-current ${STATUS_STYLES[status] || 'bg-muted-foreground'} ${
        animate ? 'animate-pulse' : ''
      }`}
    />
  );
}

export function AgentPanel() {
  const { agents, selectedAgentTab, setSelectedAgentTab, cancelAgent } = useStore();
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (selectedAgentTab && !agents.some((agent) => agent.role === selectedAgentTab)) {
      setSelectedAgentTab(null);
    }
  }, [agents, selectedAgentTab, setSelectedAgentTab]);

  if (agents.length === 0) {
    return null;
  }

  const running = agents.filter((a) => a.status === 'running').length;

  return (
    <div className="rounded-md border border-border bg-card/50 select-none">
      <div className="px-3 py-1.5">
        {/* 紧凑模式：Agent Tab 栏 */}
        <div className="flex items-center gap-1.5 text-xs">
          <button
            className="flex items-center gap-1 text-muted-foreground hover:text-foreground transition-colors shrink-0"
            onClick={() => setExpanded(!expanded)}
          >
            {expanded ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )}
            <Users className="w-3.5 h-3.5" />
            {running > 0 && (
              <span className="text-green-500">{running}</span>
            )}
          </button>

          {/* Tab 按钮：主对话 + 每个 Agent */}
          <div className="flex items-center gap-0.5 overflow-x-auto">
            <button
              className={`px-2 py-0.5 rounded transition-colors whitespace-nowrap ${
                selectedAgentTab === null
                  ? 'bg-primary/20 text-foreground font-medium'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setSelectedAgentTab(null)}
            >
              主对话
            </button>
            {agents.map((agent) => (
              <button
                key={agent.role}
                className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded transition-colors whitespace-nowrap ${
                  selectedAgentTab === agent.role
                    ? 'bg-primary/20 text-foreground font-medium'
                    : 'text-muted-foreground hover:text-foreground'
                }`}
                onClick={() => setSelectedAgentTab(
                  selectedAgentTab === agent.role ? null : agent.role
                )}
              >
                <StatusDot status={agent.status} />
                <span className="truncate max-w-[60px]">{agent.label}</span>
                {agent.status === 'running' && (
                  <span
                    role="button"
                    tabIndex={0}
                    className="ml-0.5 inline-flex h-4 w-4 items-center justify-center rounded text-red-400 hover:bg-red-500/10 hover:text-red-300"
                    title={`停止 ${agent.label}`}
                    onClick={(event) => {
                      event.stopPropagation();
                      cancelAgent(agent.role);
                    }}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault();
                        event.stopPropagation();
                        cancelAgent(agent.role);
                      }
                    }}
                  >
                    <Square className="h-3 w-3" />
                  </span>
                )}
              </button>
            ))}
          </div>
        </div>

        {/* 展开模式：详细信息 */}
        {expanded && (
          <div className="mt-1 mb-1 space-y-1">
            {agents.map((agent) => (
              <div
                key={agent.role}
                className={`flex items-center gap-2 px-2 py-1 rounded text-xs cursor-pointer transition-colors ${
                  selectedAgentTab === agent.role
                    ? 'bg-primary/10 ring-1 ring-primary/30'
                    : 'bg-muted/30 hover:bg-muted/50'
                }`}
                onClick={() => setSelectedAgentTab(
                  selectedAgentTab === agent.role ? null : agent.role
                )}
              >
                <StatusDot status={agent.status} />
                <span className="font-medium text-foreground">{agent.label}</span>
                <span className="text-muted-foreground">@{agent.role}</span>
                <span className={`ml-auto ${STATUS_TEXT_COLORS[agent.status] || 'text-muted-foreground'}`}>
                  {STATUS_LABELS[agent.status] || agent.status}
                </span>
                {agent.status === 'running' && (
                  <button
                    type="button"
                    className="inline-flex h-5 w-5 items-center justify-center rounded text-red-400 hover:bg-red-500/10 hover:text-red-300"
                    title={`停止 ${agent.label}`}
                    onClick={(event) => {
                      event.stopPropagation();
                      cancelAgent(agent.role);
                    }}
                  >
                    <Square className="h-3 w-3" />
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
