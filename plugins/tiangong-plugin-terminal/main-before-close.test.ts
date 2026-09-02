/**
 * 关闭链路验证：tab 关闭（宿主 runPluginBeforeClose）→ main.ts 注册的
 * beforeClose 处理器 → sidecar.terminalClose 精确回收该 tab 附着的终端。
 *
 * 验证点：
 * - 实例附着的终端编号与会话编号随关闭请求精确送出（多终端时只关自己的）；
 * - 隐藏执行壳（app.visible=false，无视图无附着）关闭时不发任何
 *   terminalClose——不误杀其他实例或工具持有的终端。
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('@xterm/xterm', () => {
  const TerminalMock = vi.fn().mockImplementation(() => ({
    loadAddon: vi.fn(),
    open: vi.fn(),
    reset: vi.fn(),
    focus: vi.fn(),
    write: vi.fn(),
    dispose: vi.fn(),
    onData: vi.fn(),
    buffer: { active: { baseY: 0 } },
    rows: 24,
    cols: 80,
    scrollToBottom: vi.fn(),
    refresh: vi.fn(),
  }));
  return { Terminal: TerminalMock };
});
vi.mock('@xterm/addon-fit', () => ({ FitAddon: vi.fn().mockImplementation(() => ({ fit: vi.fn() })) }));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

// SDK 桩：各用例注入 bridge 与 shadow runtime 后动态加载 main.ts。
const sdk = vi.hoisted(() => ({
  bridge: null as unknown as { call: ReturnType<typeof vi.fn>; on: ReturnType<typeof vi.fn> },
  runtime: null as unknown as Record<string, unknown>,
}));
vi.mock('@tiangong/plugin-sdk', () => ({
  createTiangongBridge: async () => sdk.bridge,
  getShadowHostRuntime: () => sdk.runtime,
}));

/** shadow runtime 桩：捕获 beforeClose 注册以便模拟宿主关闭通知。 */
function makeRuntime(context: Record<string, unknown>) {
  const beforeCloseHandlers: Array<() => void | Promise<void>> = [];
  return {
    runtime: {
      root: document,
      context,
      registerCleanup: vi.fn(),
      registerBeforeClose: (fn: () => void | Promise<void>) => {
        beforeCloseHandlers.push(fn);
      },
      onContextChange: vi.fn(() => () => {}),
    },
    beforeCloseHandlers,
  };
}

/** 挂载带真实尺寸的 #terminal-root（waitSized 依赖 clientWidth/Height）。 */
function mountRoot() {
  document.body.innerHTML = '<div id="terminal-root"></div>';
  const host = document.getElementById('terminal-root')!;
  Object.defineProperty(host, 'clientWidth', { value: 800, configurable: true });
  Object.defineProperty(host, 'clientHeight', { value: 600, configurable: true });
  return host;
}

/** bridge 桩：terminalFind 按 session_id 精确查找，不中返回 null。 */
function makeBridge(findSessionId: string | null, spawnSessionId = 'spawned-new') {
  return {
    call: vi.fn(async (method: string) => {
      if (method === 'sidecar.terminalFind') {
        return JSON.stringify({ session_id: findSessionId, history: '' });
      }
      if (method === 'sidecar.terminalSpawn') {
        return JSON.stringify({ session_id: spawnSessionId, boot_output: '' });
      }
      return '{}';
    }),
    on: vi.fn(() => () => {}),
  };
}

beforeEach(() => {
  delete (window as Record<string, unknown>).__tiangongTerminalSessions;
  vi.resetModules();
});

describe('tab 关闭通知插件并释放终端', () => {
  it('关闭前通知携带精确的会话与终端编号', async () => {
    mountRoot();
    const bridge = makeBridge('tty-9');
    sdk.bridge = bridge as unknown as typeof sdk.bridge;
    const { runtime, beforeCloseHandlers } = makeRuntime({
      session: { id: 'session-a', workspace: '/tmp' },
      app: { instance_id: 'tty-9', visible: true },
    });
    sdk.runtime = runtime;

    const main = await import('./src/main');
    // 等待异步 bootstrap 完成：视图建立并附着 tty-9。
    await vi.waitFor(() => {
      expect(main.terminalView?.sessionId()).toBe('tty-9');
    });
    expect(beforeCloseHandlers.length).toBeGreaterThan(0);

    // 模拟宿主关闭通知（TabsContainer 关 tab 前调用）。
    await Promise.all(beforeCloseHandlers.map((handler) => handler()));

    const close = bridge.call.mock.calls.find(
      ([method]: [string]) => method === 'sidecar.terminalClose',
    );
    expect(close).toBeDefined();
    expect(JSON.parse(close![1])).toEqual({
      scope_id: 'session-a',
      session_id: 'tty-9',
    });
  });

  it('隐藏执行壳关闭不发 terminalClose（不误杀任何终端）', async () => {
    mountRoot();
    const bridge = makeBridge('tty-1');
    sdk.bridge = bridge as unknown as typeof sdk.bridge;
    const { runtime, beforeCloseHandlers } = makeRuntime({
      session: { id: 'session-b', workspace: '/tmp' },
      app: { instance_id: 'tty-1', visible: false },
    });
    sdk.runtime = runtime;

    await import('./src/main');
    await vi.waitFor(() => {
      expect(beforeCloseHandlers.length).toBeGreaterThan(0);
    });
    // 后台壳不建视图、不跟随会话：关闭通知必须直接返回。
    await Promise.all(beforeCloseHandlers.map((handler) => handler()));
    expect(
      bridge.call.mock.calls.some(([method]: [string]) => method === 'sidecar.terminalClose'),
    ).toBe(false);
  });

  it('新建标签（编号无对应终端）按编号新建终端而非附着旧终端', async () => {
    mountRoot();
    // 终端精确查找不中：手动新建标签的编号（plugin-uuid）没有对应终端。
    const bridge = makeBridge(null, 'spawned-new');
    sdk.bridge = bridge as unknown as typeof sdk.bridge;
    const { runtime } = makeRuntime({
      session: { id: 'session-a', workspace: '/tmp' },
      app: { instance_id: 'plugin-manual-tab', visible: true },
    });
    sdk.runtime = runtime;

    const main = await import('./src/main');
    await vi.waitFor(() => {
      expect(main.terminalView?.sessionId()).toBe('spawned-new');
    });

    // 新建终端的 spawn 必须携带标签编号（App 实例与 PTY 一一对应）。
    const spawn = bridge.call.mock.calls.find(
      ([method]: [string]) => method === 'sidecar.terminalSpawn',
    );
    expect(spawn).toBeDefined();
    expect(JSON.parse(spawn![1]).session_id).toBe('plugin-manual-tab');
  });
});
