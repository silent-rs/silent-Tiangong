import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Message } from '@/api/tauri';
import { groupMessages } from '@/components/message';
import { StreamingMessage } from '@/components/message/StreamingMessage';

vi.mock('md-editor-rt', () => ({
  MdPreview: ({ modelValue }: { modelValue: string }) => (
    <div data-testid="md-preview">{modelValue}</div>
  ),
}));

function message(
  id: string,
  role: Message['role'],
  text: string,
  extra: Partial<Message> = {},
): Message {
  return {
    id,
    role,
    content: [{ type: 'text', text }],
    reasoning_content: '',
    phase: 'normal',
    created_at: '2026-09-13 00:00:00',
    ...extra,
  };
}

describe('groupMessages 引用复用（流式期间历史组保持稳定）', () => {
  it('消息引用不变时组对象复用，全组命中时沿用同一分组数组', () => {
    const messages = [
      message('u1', 'user', '问题'),
      message('a1', 'assistant', '回答'),
    ];
    const first = groupMessages(messages);
    const second = groupMessages([...messages]);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).toBe(first[1]);
    // 全部命中：分组数组本身也复用，派生 useMemo 保持命中。
    expect(second).toBe(first);
  });

  it('组内消息被替换引用（流式增量）时该组重建、其他组复用', () => {
    const m1 = message('u1', 'user', '问题');
    const m2 = message('a1', 'assistant', '');
    const first = groupMessages([m1, m2]);
    // 流式批次：store 以不可变更新产生新消息对象（id 不变、引用变）。
    const m2Next = { ...m2, content: [{ type: 'text', text: '流式增量' }] } as Message;
    const second = groupMessages([m1, m2Next]);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).not.toBe(first[1]);
    expect(second[1].messages[0]).toBe(m2Next);
  });

  it('组内消息增减（新消息加入本轮）时该组重建', () => {
    const m1 = message('u1', 'user', '问题');
    const m2 = message('a1', 'assistant', '回答');
    const first = groupMessages([m1, m2]);
    const m3 = message('t1', 'tool', '结果');
    const second = groupMessages([m1, m2, m3]);
    expect(second[0]).toBe(first[0]);
    expect(second[1]).not.toBe(first[1]);
    expect(second[1].messages).toHaveLength(2);
  });

  it('切换会话再切回不串组（缓存整体替换，跨会话不误命中）', () => {
    const sessionA = [message('ua', 'user', 'A 的问题'), message('aa', 'assistant', 'A 的回答')];
    const sessionB = [message('ub', 'user', 'B 的问题'), message('ab', 'assistant', 'B 的回答')];
    groupMessages(sessionA);
    groupMessages(sessionB);
    const back = groupMessages(sessionA);
    expect(back.map((g) => g.messages[0].id)).toEqual(['ua', 'aa']);
    expect(back[0].messages[0].content).toEqual(sessionA[0].content);
  });

  it('worker 连续消息聚合进同组、compressedresume 仍被跳过', () => {
    const w1 = message('w1', 'tool', '一', { worker_id: 'agent:x:1' } as Partial<Message>);
    const w2 = message('w2', 'tool', '二', { worker_id: 'agent:x:1' } as Partial<Message>);
    const groups = groupMessages([
      w1,
      w2,
      message('r1', 'assistant', '续接', { phase: 'compressedresume' }),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].type).toBe('worker');
    expect(groups[0].messages).toHaveLength(2);
    // 追加消息后同 key 组重建，聚合规则不变。
    const w3 = message('w3', 'tool', '三', { worker_id: 'agent:x:1' } as Partial<Message>);
    const next = groupMessages([w1, w2, w3]);
    expect(next[0].messages).toHaveLength(3);
    expect(next[0].key).toBe(groups[0].key);
  });
});

describe('StreamingMessage 流式渲染节流', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    vi.useFakeTimers();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
  });

  const previewText = () =>
    container.querySelector('[data-testid="md-preview"]')?.textContent ?? '';

  const renderStreaming = (content: string) =>
    act(() => root.render(<StreamingMessage content={content} reasoningContent="" />));

  const rerenderStreaming = (content: string) =>
    act(() => root.render(<StreamingMessage content={content} reasoningContent="" />));

  it('间隔内多个增量只渲染一次，到期对齐最新内容', () => {
    renderStreaming('A');
    expect(previewText()).toBe('A');

    rerenderStreaming('AB');
    rerenderStreaming('ABC');
    expect(previewText()).toBe('A');

    act(() => vi.advanceTimersByTime(149));
    expect(previewText()).toBe('A');
    act(() => vi.advanceTimersByTime(1));
    expect(previewText()).toBe('ABC');
  });

  it('内容回退（编辑重发等长度变短）立即同步显示', () => {
    renderStreaming('ABCDE');
    act(() => vi.advanceTimersByTime(150));
    expect(previewText()).toBe('ABCDE');

    rerenderStreaming('X');
    expect(previewText()).toBe('X');
  });

  it('卸载后无残留定时器', () => {
    renderStreaming('A');
    rerenderStreaming('AB');
    act(() => root.unmount());
    expect(vi.getTimerCount()).toBe(0);
  });
});
