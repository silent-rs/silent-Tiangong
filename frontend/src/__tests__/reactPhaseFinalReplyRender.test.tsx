import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { AgentTurn } from '@/components/message/AgentTurn';
import { setupMarkdownLinkify } from '@/utils/markdownLinkify';
import type { Message } from '@/api/tauri';

setupMarkdownLinkify();

/** 复刻内存态消息：后端自「任务 15」起最终回复以 ReactText 流式下发，
 * 前端消息 phase 停留在 react（落盘才是 summary）。 */
function reactPhaseMessages(): Message[] {
  const mk = (id: string, phase: string, text: string): Message => ({
    id,
    role: 'assistant',
    content: [{ type: 'text', text }],
    reasoning_content: '',
    phase: phase as Message['phase'],
    created_at: '2026-09-15 19:00:00',
  });
  return [
    mk('final-a', 'react', '先查一下数据。'),
    {
      id: 'final-tool',
      role: 'tool',
      content: [{ type: 'text', text: 'ok' }],
      reasoning_content: '',
      created_at: '2026-09-15 19:00:01',
    } as unknown as Message,
    mk('final-b', 'react', '## 已完成\n\n**结论**：验证通过。\n\n- 第一项\n- 第二项\n\n| 列A | 列B |\n|---|---|\n| 1 | 2 |'),
  ];
}

describe('已完成轮次的最终回复按总结渲染', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it('phase=react 的最终回复在完成轮渲染为 Markdown', async () => {
    await act(async () => {
      root.render(
        <AgentTurn
          messages={reactPhaseMessages()}
          streamingMessageId={null}
          streamingContent=""
          streamingReasoningContent=""
          hasTts={false}
        />,
      );
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 80)); });

    expect(container.querySelectorAll('h2').length).toBe(1);
    expect(container.querySelectorAll('strong').length).toBe(1);
    expect(container.querySelectorAll('table').length).toBe(1);
    expect(container.textContent).toContain('结论：验证通过');
  });

  it('活跃轮保持现状：react 文本不提级', async () => {
    await act(async () => {
      root.render(
        <AgentTurn
          messages={reactPhaseMessages()}
          streamingMessageId={null}
          streamingContent=""
          streamingReasoningContent=""
          hasTts={false}
          isActive
        />,
      );
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 80)); });

    expect(container.querySelectorAll('h2').length).toBe(0);
    expect(container.querySelectorAll('table').length).toBe(0);
  });
});
