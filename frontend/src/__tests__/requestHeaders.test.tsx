import { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RequestHeadersEditor } from '@/components/RequestHeadersEditor';

let root: Root;
let container: HTMLDivElement;
let latest: Record<string, string>;

function Editor() {
  const [headers, setHeaders] = useState<Record<string, string>>({});
  return <RequestHeadersEditor headers={headers} onChange={(next) => { latest = next; setHeaders(next); }} />;
}

beforeEach(async () => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
  container = document.createElement('div');
  document.body.appendChild(container);
  root = createRoot(container);
  latest = {};
  await act(async () => root.render(<Editor />));
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

async function edit(label: string, value: string) {
  const input = container.querySelector(`[aria-label="${label}"]`) as HTMLInputElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

describe('请求头设置', () => {
  it('支持通用名称和值的添加、修改和删除，不提供服务商快捷入口', async () => {
    expect(container.textContent).not.toContain('OpenCode');
    await act(async () => (container.querySelector('[aria-label="添加请求头"]') as HTMLButtonElement).click());
    await edit('请求头值 1', '${session_id}');
    await edit('请求头名称 1', 'x-test-session');
    expect(latest).toEqual({ 'x-test-session': '${session_id}' });
    await act(async () => (container.querySelector('[aria-label="删除请求头 1"]') as HTMLButtonElement).click());
    expect(latest).toEqual({});
  });

  it('帮助提示说明 session_id 占位符', async () => {
    await act(async () => (container.querySelector('[aria-label="请求头帮助"]') as HTMLButtonElement).focus());
    expect(document.querySelector('[role="tooltip"]')?.textContent).toContain('${session_id}');
    expect(document.querySelector('[role="tooltip"]')?.textContent).toContain('同一会话内保持一致');
  });
});
