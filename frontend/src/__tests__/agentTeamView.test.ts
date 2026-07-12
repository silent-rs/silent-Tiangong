import { describe, expect, it } from 'vitest';

import type { Message } from '@/api/tauri';
import { workerBelongsToAgent } from '@/components/message/utils';
import { parseAgentsFromMessages } from '@/store/useStore';

function systemMessage(id: string, text: string): Message {
  return {
    id,
    role: 'system',
    content: [{ type: 'text', text }],
    reasoning_content: '',
    created_at: '2026-07-12 00:00:00',
  };
}

describe('agent team view routing', () => {
  it('updates equal-label agents by exact agent id', () => {
    const created = [
      systemMessage('create-dev', '[Agent] Worker (dev) 已加入团队 id=agent-dev'),
      systemMessage('create-test', '[Agent] Worker (test) 已加入团队 id=agent-test'),
    ];
    const running = parseAgentsFromMessages([
      ...created,
      systemMessage(
        'agent-status:agent-test',
        '[Agent] Worker 状态变更: running id=agent-test',
      ),
    ]);

    expect(running.find((agent) => agent.agentId === 'agent-dev')?.status).toBe('idle');
    expect(running.find((agent) => agent.agentId === 'agent-test')?.status).toBe('running');

    const terminated = parseAgentsFromMessages([
      ...created,
      systemMessage(
        'agent-status:agent-test',
        '[Agent] Worker 状态变更: terminated id=agent-test',
      ),
    ]);
    expect(terminated.map((agent) => agent.agentId)).toEqual(['agent-dev']);
  });

  it('does not mix a recreated role with the dismissed agent history', () => {
    expect(workerBelongsToAgent('agent:dev:old-agent', 'dev', 'new-agent')).toBe(false);
    expect(workerBelongsToAgent('agent:dev:new-agent', 'dev', 'new-agent')).toBe(true);
    expect(workerBelongsToAgent('agent:test:new-agent', 'dev', 'new-agent')).toBe(false);
  });
});
