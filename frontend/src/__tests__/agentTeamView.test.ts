import { describe, expect, it } from 'vitest';

import type { Message } from '@/api/tauri';
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
    useStore.setState({
      activeSessionId: 'session-main',
      isNewConversation: false,
      messages: [reactMessage],
      runStatus: 'idle',
      streamingMessageId: null,
      streamingContent: '',
      streamingReasoningContent: '',
    });
    useStore.getState().applyStreamEvents([{
      session_id: 'session-main',
      event: { type: 'session_message_upsert', message: summaryMessage },
    }]);

    const [result] = useStore.getState().messages;
    expect(result).not.toBe(reactMessage);
    expect(result.phase).toBe('summary');
  });

  it('keeps message content changes and checks the requested streaming id', () => {
    const sessionId = 'session-model-exclusion';
    const visible = systemMessage('agent-process', '执行过程');
    const excluded = { ...visible, content: [{ type: 'text', text: '执行过程（更新）' }] };

    useStore.setState({
      activeSessionId: sessionId,
      isNewConversation: false,
      messages: [visible],
      runStatus: 'idle',
    });
    useStore.getState().applyStreamEvents([{
      session_id: sessionId,
      event: { type: 'session_message_upsert', message: excluded },
    }]);

    expect(useStore.getState().messages[0].content).toEqual(excluded.content);
    expect(hasMessage([visible], 'missing')).toBe(false);
    expect(hasMessage([visible], visible.id)).toBe(true);
  });
});
