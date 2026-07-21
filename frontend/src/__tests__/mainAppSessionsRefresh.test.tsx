import { act, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { Session } from '@/api/tauri';

const mocks = vi.hoisted(() => {
  const eventHandlers = new Map<string, Set<(event: { payload: unknown }) => void>>();
  const unlisteners: ReturnType<typeof vi.fn>[] = [];
  const makeUnlistener = () => {
    const unlisten = vi.fn();
    unlisteners.push(unlisten);
    return unlisten;
  };
  const appWindow = {
    scaleFactor: vi.fn(() => Promise.resolve(1)),
    innerSize: vi.fn(() => Promise.resolve({ width: 1200, height: 800 })),
    setSize: vi.fn(() => Promise.resolve()),
    onResized: vi.fn(() => Promise.resolve(makeUnlistener())),
  };
  const api = {
    getSessions: vi.fn(),
    getReasoningEffort: vi.fn(() => Promise.resolve('medium')),
    getWorkspaceDir: vi.fn(() => Promise.resolve('/workspace')),
    onStreamEvent: vi.fn(() => Promise.resolve(makeUnlistener())),
    browserHide: vi.fn(() => Promise.resolve()),
  };
  const listen = vi.fn((eventName: string, handler: (event: { payload: unknown }) => void) => {
    let handlers = eventHandlers.get(eventName);
    if (!handlers) {
      handlers = new Set();
      eventHandlers.set(eventName, handlers);
    }
    handlers.add(handler);
    const unlisten = vi.fn(() => handlers!.delete(handler));
    unlisteners.push(unlisten);
    return Promise.resolve(unlisten);
  });
  return { api, appWindow, eventHandlers, listen, makeUnlistener, unlisteners };
});

vi.mock('@/api/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/tauri')>();
  return { ...actual, api: { ...actual.api, ...mocks.api } };
});

vi.mock('@tauri-apps/api/event', () => ({ listen: mocks.listen }));
vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor: vi.fn(() => Promise.resolve(null)),
  getCurrentWindow: vi.fn(() => mocks.appWindow),
  LogicalSize: class LogicalSize {
    constructor(public width: number, public height: number) {}
  },
}));
vi.mock('@/components/AppSidebar', () => ({ AppSidebar: () => null }));
vi.mock('@/components/LazyComponents', () => ({
  LazyMessageInput: () => null,
  LazyMessageList: () => null,
  LazyStatusPanel: () => null,
}));
vi.mock('@/components/TabsContainer', () => ({ TabsContainer: () => null }));
vi.mock('@/components/ui/sidebar', () => ({
  SidebarProvider: ({ children }: { children: ReactNode }) => children,
}));
vi.mock('@/hooks/useUpdateCheck', () => ({ useUpdateCheck: () => undefined }));
vi.mock('@/utils/desktopNotification', () => ({
  ensureDesktopNotificationPermission: vi.fn(() => Promise.resolve()),
}));

const { MainApp } = await import('@/pages/MainApp');
const { useStore } = await import('@/store/useStore');
const initialState = useStore.getInitialState();
const getSessionsMock = vi.mocked(mocks.api.getSessions);

function session(id: string, messageCount: number): Session {
  return {
    id,
    title: id,
    created_at: '2026-07-21 00:00:00',
    updated_at: '2026-07-21 00:00:00',
    message_count: messageCount,
    cwd: '/tmp',
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

async function flushMicrotasks() {
  for (let index = 0; index < 30; index += 1) {
    await Promise.resolve();
  }
}

function emitSessionsUpdated() {
  for (const handler of mocks.eventHandlers.get('sessions_updated') ?? []) {
    handler({ payload: undefined });
  }
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mountMainApp() {
  container = document.createElement('div');
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<MainApp />);
    await flushMicrotasks();
  });
  expect(mocks.eventHandlers.get('sessions_updated')?.size).toBe(1);
  getSessionsMock.mockClear();
}

describe('MainApp sessions_updated scheduling contract', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.eventHandlers.clear();
    mocks.unlisteners.length = 0;
    mocks.listen.mockClear();
    mocks.appWindow.onResized.mockClear();
    mocks.api.onStreamEvent.mockClear();
    mocks.api.getWorkspaceDir.mockClear();
    mocks.api.getReasoningEffort.mockClear();
    mocks.api.getReasoningEffort.mockResolvedValue('medium');
    getSessionsMock.mockReset();
    getSessionsMock.mockResolvedValue([session('a', 1)]);
    useStore.setState({
      ...initialState,
      sessions: [session('a', 1)],
      activeSessionId: 'a',
      isNewConversation: false,
    }, true);
  });

  afterEach(async () => {
    if (root) {
      await act(async () => root!.unmount());
    }
    root = null;
    container?.remove();
    container = null;
    vi.clearAllTimers();
    vi.useRealTimers();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it('一次事件突发只触发一次 protective 刷新', async () => {
    await mountMainApp();

    act(() => emitSessionsUpdated());
    await advance(40);
    act(() => emitSessionsUpdated());
    await advance(79);
    act(() => emitSessionsUpdated());
    // 尾沿去抖：最后一次事件静默 120ms 后才触发，故需再推进一个完整窗口。
    await advance(120);

    expect(getSessionsMock).toHaveBeenCalledTimes(1);
  });

  it('去抖应等待最后一个事件静默 120ms 后再刷新', async () => {
    await mountMainApp();

    act(() => emitSessionsUpdated());
    await advance(119);
    act(() => emitSessionsUpdated());
    await advance(1);
    const callsAtFirstWindow = getSessionsMock.mock.calls.length;
    await advance(119);

    expect(callsAtFirstWindow).toBe(0);
    expect(getSessionsMock).toHaveBeenCalledTimes(1);
  });

  it('后端返回错误时保留旧列表，下一次事件重新刷新后收敛', async () => {
    // 请求失败时保留旧列表，收敛由下一次 sessions_updated 事件自然触发。
    await mountMainApp();
    getSessionsMock
      .mockRejectedValueOnce(new Error('read_dir failed'))
      .mockResolvedValueOnce([session('a', 2)]);
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);

    act(() => emitSessionsUpdated());
    await advance(120);
    // 失败时保留挂载时的旧列表，不被错误清空。
    expect(useStore.getState().sessions[0].message_count).toBe(1);
    expect(getSessionsMock).toHaveBeenCalledTimes(1);

    act(() => emitSessionsUpdated());
    await advance(120);
    expect(getSessionsMock).toHaveBeenCalledTimes(2);
    expect(useStore.getState().sessions[0].message_count).toBe(2);
    consoleError.mockRestore();
  });

  it('刷新进行中收到新事件时不并发读取，并在 settle 后执行一次 dirty rerun', async () => {
    await mountMainApp();
    const first = deferred<Session[]>();
    const second = deferred<Session[]>();
    let inFlight = 0;
    let maxInFlight = 0;
    getSessionsMock
      .mockImplementationOnce(async () => {
        inFlight += 1;
        maxInFlight = Math.max(maxInFlight, inFlight);
        const result = await first.promise;
        inFlight -= 1;
        return result;
      })
      .mockImplementationOnce(async () => {
        inFlight += 1;
        maxInFlight = Math.max(maxInFlight, inFlight);
        const result = await second.promise;
        inFlight -= 1;
        return result;
      });

    act(() => emitSessionsUpdated());
    await advance(120);
    act(() => emitSessionsUpdated());
    await advance(120);
    const observedMaxInFlight = maxInFlight;

    first.resolve([session('a', 2)]);
    second.resolve([session('a', 3)]);
    await act(async () => flushMicrotasks());

    expect(observedMaxInFlight).toBe(1);
    expect(getSessionsMock).toHaveBeenCalledTimes(2);
  });

  it('unmount 会取消 pending timer 并注销所有已注册 listener', async () => {
    await mountMainApp();
    act(() => emitSessionsUpdated());
    const registeredUnlisteners = [...mocks.unlisteners];

    await act(async () => root!.unmount());
    root = null;
    await advance(120);

    expect(getSessionsMock).not.toHaveBeenCalled();
    expect(registeredUnlisteners.length).toBe(6);
    for (const unlisten of registeredUnlisteners) {
      expect(unlisten).toHaveBeenCalledTimes(1);
    }
    expect(mocks.eventHandlers.get('sessions_updated')?.size ?? 0).toBe(0);
  });
});
