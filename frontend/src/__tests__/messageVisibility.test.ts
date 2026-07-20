import { describe, expect, it } from 'vitest';
import type { Message } from '@/api/tauri';
import { groupMessages } from '@/components/message';
import { findSearchMatches } from '@/utils/search';

function message(
  id: string,
  role: Message['role'],
  text: string,
  phase: Message['phase'] = 'normal',
): Message {
  return {
    id,
    role,
    content: [{ type: 'text', text }],
    reasoning_content: '',
    phase,
    created_at: '2026-07-19 00:00:00',
  };
}

describe('压缩续接消息可见性', () => {
  const messages = [
    message('user-1', 'user', '第一条真实问题'),
    message('assistant-1', 'assistant', '第一条真实回答'),
    message('resume', 'user', '仅供模型使用的续接内容', 'compressedresume'),
    message('user-2', 'user', '第二条真实问题'),
    message('assistant-2', 'assistant', '第二条真实回答'),
  ];

  it('分组时只隐藏续接消息并保留完整真实对话', () => {
    const visibleIds = groupMessages(messages).flatMap((group) =>
      group.messages.map((item) => item.id),
    );

    expect(visibleIds).toEqual(['user-1', 'assistant-1', 'user-2', 'assistant-2']);
  });

  it('所有搜索范围都排除续接消息', () => {
    expect(findSearchMatches(messages, '续接内容', [], 'messages')).toEqual([]);
    expect(findSearchMatches(messages, '续接内容', [], 'withThinking')).toEqual([]);
    expect(findSearchMatches(messages, '续接内容', [], 'all')).toEqual([]);
    expect(findSearchMatches(messages, '第二条真实问题', [], 'messages')).toHaveLength(1);
  });
});
