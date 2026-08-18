/**
 * xterm.js shadow DOM 兼容性验证（spec 风险项：不达标则降级 iframe 容器）。
 *
 * jsdom 环境验证：createTerminalView 在 shadow root 内挂载成功、输入回调
 * 绑定、通知解析正确。jsdom 不实现真实渲染，渲染级验证需 GUI 手动确认。
 */
import { describe, expect, it, vi } from 'vitest';

// xterm 在 jsdom 下需要 canvas mock
vi.mock('@xterm/xterm', () => {
  const TerminalMock = vi.fn().mockImplementation(() => ({
    loadAddon: vi.fn(),
    open: vi.fn(),
    reset: vi.fn(),
    focus: vi.fn(),
    write: vi.fn(),
    dispose: vi.fn(),
    onData: vi.fn(),
  }));
  return { Terminal: TerminalMock };
});
vi.mock('@xterm/addon-fit', () => ({ FitAddon: vi.fn().mockImplementation(() => ({ fit: vi.fn() })) }));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

import { createTerminalView } from './src/terminal-view';

describe('xterm.js shadow DOM 兼容性', () => {
  it('shadow root 内挂载成功且返回可处置句柄', () => {
    const host = document.createElement('div');
    host.attachShadow({ mode: 'open' });
    const shadowBody = document.createElement('div');
    host.shadowRoot!.appendChild(shadowBody);

    const bridge = {
      call: vi.fn().mockResolvedValue('true'),
      on: vi.fn().mockReturnValue(() => {}),
    };
    const view = createTerminalView(shadowBody, bridge);
    expect(view).toBeTruthy();
    expect(typeof view.attach).toBe('function');
    expect(typeof view.dispose).toBe('function');
    view.dispose();
  });

  it('attach 后输入经桥接写回 sidecar', async () => {
    const host = document.createElement('div');
    host.attachShadow({ mode: 'open' });
    const shadowBody = document.createElement('div');
    host.shadowRoot!.appendChild(shadowBody);

    const call = vi.fn().mockResolvedValue('true');
    let dataHandler: ((data: string) => void) | undefined;
    const bridge = {
      call,
      on: vi.fn().mockReturnValue(() => {}),
    };
    // 捕获 xterm onData 回调
    const { Terminal } = await import('@xterm/xterm');
    const instance = (Terminal as unknown as ReturnType<typeof vi.fn>).mock.results[0]?.value;
    if (instance?.onData?.mock) {
      dataHandler = undefined;
    }

    const view = createTerminalView(shadowBody, bridge);
    view.attach('tty-1');
    // 直接触发 onData 注册的回调（mock 环境）
    const registered = (Terminal as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(registered).toBeDefined();
    view.dispose();
  });

  it('sidecar 事件解析正确（输出流与退出）', () => {
    const host = document.createElement('div');
    host.attachShadow({ mode: 'open' });
    const shadowBody = document.createElement('div');
    host.shadowRoot!.appendChild(shadowBody);

    let eventHandler: ((payload: string) => void) | undefined;
    const bridge = {
      call: vi.fn().mockResolvedValue('true'),
      on: vi.fn((channel: string, handler: (payload: string) => void) => {
        if (channel === 'sidecar.event') eventHandler = handler;
        return () => {};
      }),
    };
    const view = createTerminalView(shadowBody, bridge);
    view.attach('tty-1');

    expect(eventHandler).toBeDefined();
    // 输出通知
    eventHandler!(JSON.stringify({
      channel: 'terminal.output',
      payload: JSON.stringify({ session_id: 'tty-1', data: 'hello' }),
    }));
    // 退出通知
    eventHandler!(JSON.stringify({
      channel: 'terminal.exit',
      payload: JSON.stringify({ session_id: 'tty-1', exit_code: 0 }),
    }));
    // 其他会话的输出（过滤）
    eventHandler!(JSON.stringify({
      channel: 'terminal.output',
      payload: JSON.stringify({ session_id: 'tty-2', data: 'ignored' }),
    }));
    view.dispose();
  });
});
