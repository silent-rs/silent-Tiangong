import { describe, expect, it } from 'vitest';

import type { Message, RunSnapshot } from '@/api/tauri';
import { hasMessage, workerBelongsToAgent } from '@/components/message/utils';
import { parseAgentsFromMessages, useStore } from '@/store/useStore';

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

  it('replaces a react message with the summary result when only phase changes', () => {
    const reactMessage: Message = {
      id: 'main-result',
      role: 'assistant',
      content: [{ type: 'text', text: '最终审查结果' }],
      reasoning_content: '',
      phase: 'react',
      created_at: '2026-07-12 00:00:01',
    };
    const summaryMessage: Message = { ...reactMessage, phase: 'summary' };
    const snapshot: RunSnapshot = {
      status: 'idle',
      last_session_id: 'session-main',
      messages: [summaryMessage],
      input_draft: '',
      pending_session_ids: [],
    };

    useStore.setState({
      activeSessionId: 'session-main',
      isDraft: false,
      messages: [reactMessage],
      runStatus: 'idle',
      streamingMessageId: null,
      streamingContent: '',
      streamingReasoningContent: '',
    });
    useStore.getState().updateFromSnapshot(snapshot);

    const [result] = useStore.getState().messages;
    expect(result).not.toBe(reactMessage);
    expect(result.phase).toBe('summary');
  });

  it('keeps model exclusion changes and checks the requested streaming id', () => {
    const visible = systemMessage('agent-process', '执行过程');
    const excluded = { ...visible, model_excluded: true };

    useStore.setState({
      activeSessionId: 'session-main',
      isDraft: false,
      messages: [visible],
      runStatus: 'idle',
    });
    useStore.getState().updateFromSnapshot({
      status: 'idle',
      last_session_id: 'session-main',
      messages: [excluded],
      input_draft: '',
      pending_session_ids: [],
    });

    expect(useStore.getState().messages[0].model_excluded).toBe(true);
    expect(hasMessage([visible], 'missing')).toBe(false);
    expect(hasMessage([visible], visible.id)).toBe(true);
  });
});
