import { describe, expect, it } from 'vitest';
import type { Message } from '@/api/tauri';
import { groupMessages, liveRunStartIndex } from '@/components/message';

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
    created_at: '2026-09-20 00:00:00',
    ...extra,
  };
}

describe('当前执行链起始组下标（引导消息不结束轮次）', () => {
  it('执行中注入引导消息：前序过程与引导消息同属当前执行链', () => {
    const groups = groupMessages([
      message('a1', 'user', '原始任务'),
      message('t1', 'assistant', '前序过程'),
      message('b1', 'user', '引导消息'),
      message('t2', 'assistant', '后续过程'),
    ]);
    // 组序列：[user a1][turn t1][user b1][turn t2]，链起点是原始锚点 a1。
    expect(groups.map((g) => g.type)).toEqual(['user', 'agent_turn', 'user', 'agent_turn']);
    expect(liveRunStartIndex(groups)).toBe(0);
  });

  it('多级引导链：链上最早未结算用户消息（原始锚点）为起点', () => {
    const groups = groupMessages([
      message('a2', 'user', '原始任务'),
      message('t3', 'assistant', '过程一'),
      message('b2', 'user', '引导一'),
      message('t4', 'assistant', '过程二'),
      message('b3', 'user', '引导二'),
      message('t5', 'assistant', '过程三'),
    ]);
    expect(liveRunStartIndex(groups)).toBe(0);
  });

  it('历史已结算轮次不进链：新执行只覆盖自身锚点之后', () => {
    const groups = groupMessages([
      message('h1', 'user', '历史问题', { turn_status: 'success', elapsed_ms: 1200 }),
      message('h2', 'assistant', '历史过程'),
      message('a3', 'user', '新任务'),
      message('t6', 'assistant', '新过程'),
    ]);
    // 组序列：[user h1][turn h2][user a3][turn t6]，链起点是新锚点 a3（下标 2）。
    expect(liveRunStartIndex(groups)).toBe(2);
  });

  it('ALR-107 终态写在引导消息上：原始锚点残留无终态也不扩大下一轮的链', () => {
    const groups = groupMessages([
      message('a4', 'user', '原始任务'),
      message('t7', 'assistant', '过程'),
      // 上一轮的引导消息承载终态；原始锚点 a4 保持无终态。
      message('b4', 'user', '引导消息', { turn_status: 'success', elapsed_ms: 9000 }),
      message('t8', 'assistant', '上一轮收尾'),
      message('a5', 'user', '再起新任务'),
      message('t9', 'assistant', '新过程'),
    ]);
    // 向前扫描到已结算的 b4 即停，链起点是 a5（下标 4），而非残留的 a4。
    expect(liveRunStartIndex(groups)).toBe(4);
  });

  it('全部轮次已结算：无活跃组，返回组数量', () => {
    const groups = groupMessages([
      message('h3', 'user', '问题', { turn_status: 'failed', elapsed_ms: 300 }),
      message('h4', 'assistant', '过程'),
    ]);
    expect(liveRunStartIndex(groups)).toBe(groups.length);
  });

  it('空分组返回 0', () => {
    expect(liveRunStartIndex([])).toBe(0);
  });
});
