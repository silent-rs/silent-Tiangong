import { describe, expect, it } from 'vitest';
import type { Message } from '@/api/tauri';
import { displayTextContent, isNeedMoreWorkMessage } from '@/components/message/utils';

function assistantMessage(text: string): Message {
  return {
    id: 'assistant-marker-test',
    role: 'assistant',
    content: [{ type: 'text', text }],
    reasoning_content: '',
    phase: 'react',
    created_at: '2026-09-01 00:00:00',
  };
}

describe('助手状态标记', () => {
  it('按前导空白、忽略大小写的前缀规则识别 NEED_MORE_WORK', () => {
    expect(isNeedMoreWorkMessage(assistantMessage('\n  [need_more_work]继续执行'))).toBe(true);
  });

  it('ASK_USER 不再作为控制标记剥离', () => {
    expect(displayTextContent(assistantMessage('[ASK_USER] 请确认'))).toBe('[ASK_USER] 请确认');
  });
});
