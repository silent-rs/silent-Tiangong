/**
 * Terminal GC 链路验证：tab 新建/关闭维护前端存活集合，关闭处理器向
 * sidecar.terminalGc 提交集合后立即返回，后台异常不阻止标签删除。
 *
 * 验证点：
 * - 前台与隐藏的精确终端实例都参与对账；
 * - 无实例编号的通用隐藏壳不参与，不误杀工具持有的终端。
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
function makeBridge(
  findSessionId: string | null,
  spawnSessionId = 'spawned-new',
  failGc = false,
) {
  return {
    call: vi.fn(async (method: string) => {
      if (method === 'sidecar.terminalGc' && failGc) {
        throw new Error('sidecar unavailable');
      }
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
  const timers = (window as Record<string, unknown>).__tiangongTerminalGcTimers as
    Map<string, number> | undefined;
  timers?.forEach((timer) => window.clearTimeout(timer));
  delete (window as Record<string, unknown>).__tiangongTerminalSessions;
  delete (window as Record<string, unknown>).__tiangongTerminalFrontendTabs;
  delete (window as Record<string, unknown>).__tiangongTerminalGcTimers;
  vi.resetModules();
});

describe('Terminal GC 前端存活集合对账', () => {
  it('关闭唯一终端时提交空集合且不等待后台结果', async () => {
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

    const gc = bridge.call.mock.calls.find(
      ([method]: [string]) => method === 'sidecar.terminalGc',
    );
    expect(gc).toBeDefined();
    expect(JSON.parse(gc![1])).toEqual({
      session_id: 'session-a',
      live_terminal_ids: [],
    });
  });

  it('GC 请求失败也不阻止关闭处理完成', async () => {
    mountRoot();
    const bridge = makeBridge('tty-9', 'spawned-new', true);
    sdk.bridge = bridge as unknown as typeof sdk.bridge;
    const { runtime, beforeCloseHandlers } = makeRuntime({
      session: { id: 'session-a', workspace: '/tmp' },
      app: { instance_id: 'tty-9', visible: true },
    });
    sdk.runtime = runtime;

    const main = await import('./src/main');
    await vi.waitFor(() => expect(main.terminalView?.sessionId()).toBe('tty-9'));
    await expect(
      Promise.all(beforeCloseHandlers.map((handler) => handler())),
    ).resolves.toBeDefined();
  });

  it('隐藏但有精确编号的终端关闭时同样提交 GC', async () => {
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
    await Promise.all(beforeCloseHandlers.map((handler) => handler()));
    const gc = bridge.call.mock.calls.find(
      ([method]: [string]) => method === 'sidecar.terminalGc',
    );
    expect(gc).toBeDefined();
    expect(JSON.parse(gc![1])).toEqual({
      session_id: 'session-b',
      live_terminal_ids: [],
    });
  });

  it('无实例编号的通用隐藏壳不触发 Terminal GC', async () => {
    mountRoot();
    const bridge = makeBridge(null);
    sdk.bridge = bridge as unknown as typeof sdk.bridge;
    const { runtime, beforeCloseHandlers } = makeRuntime({
      session: { id: 'session-b', workspace: '/tmp' },
      app: { visible: false },
    });
    sdk.runtime = runtime;

    await import('./src/main');
    await vi.waitFor(() => expect(beforeCloseHandlers.length).toBeGreaterThan(0));
    await Promise.all(beforeCloseHandlers.map((handler) => handler()));
    expect(
      bridge.call.mock.calls.some(([method]: [string]) => method === 'sidecar.terminalGc'),
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

  it('多标签关闭只从存活集合移除目标终端', async () => {
    const bridge = makeBridge(null);
    const shell = await import('./src/shell');
    shell.registerFrontendTerminalTab(
      bridge as unknown as Parameters<typeof shell.registerFrontendTerminalTab>[0],
      'session-a',
      'tty-1',
    );
    shell.registerFrontendTerminalTab(
      bridge as unknown as Parameters<typeof shell.registerFrontendTerminalTab>[0],
      'session-a',
      'tty-2',
    );
    shell.closeFrontendTerminalTab(
      bridge as unknown as Parameters<typeof shell.closeFrontendTerminalTab>[0],
      'session-a',
      'tty-1',
    );

    const calls = bridge.call.mock.calls.filter(
      ([method]: [string]) => method === 'sidecar.terminalGc',
    );
    expect(calls).toHaveLength(1);
    expect(JSON.parse(calls[0][1])).toEqual({
      session_id: 'session-a',
      live_terminal_ids: ['tty-2'],
    });
  });

  it('新建标签合并后提交完整存活集合', async () => {
    vi.useFakeTimers();
    try {
      const bridge = makeBridge(null);
      const shell = await import('./src/shell');
      const typedBridge = bridge as unknown as Parameters<
        typeof shell.registerFrontendTerminalTab
      >[0];
      shell.registerFrontendTerminalTab(typedBridge, 'session-a', 'tty-2');
      shell.registerFrontendTerminalTab(typedBridge, 'session-a', 'tty-1');
      await vi.advanceTimersByTimeAsync(500);

      const calls = bridge.call.mock.calls.filter(
        ([method]: [string]) => method === 'sidecar.terminalGc',
      );
      expect(calls).toHaveLength(1);
      expect(JSON.parse(calls[0][1])).toEqual({
        session_id: 'session-a',
        live_terminal_ids: ['tty-1', 'tty-2'],
      });
    } finally {
      vi.useRealTimers();
    }
  });
});
