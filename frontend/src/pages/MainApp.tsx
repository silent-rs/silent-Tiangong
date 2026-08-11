import { useEffect, useRef, useState, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import {
  api,
  type AvailablePlugin,
  type SessionStreamEvent,
  type TabKind,
  type TabState,
  type TerminalTabInfo,
} from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { DefaultPluginOnboarding } from '@/components/DefaultPluginOnboarding';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { TabsContainer } from '@/components/TabsContainer';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import { useUpdateCheck } from '@/hooks/useUpdateCheck';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize, currentMonitor } from '@tauri-apps/api/window';

/** 根据窗口逻辑高度计算 4:3 浏览器宽度 */
function calcBrowserWidth(logicalHeight: number): number {
  const available = logicalHeight - 40 - 44;
  return Math.max(200, Math.floor(available * 4 / 3));
}

/** 浏览器面板最小宽度，低于此值自动关闭 */
const MIN_BROWSER_WIDTH = 200;

/** 对话面板最小宽度 */
const MIN_CHAT_WIDTH = 400;

/** 侧边栏自动隐藏/恢复阈值 */
const SIDEBAR_RESTORE_THRESHOLD = 656;

/** 侧边栏宽度：与 CSS 变量 --sidebar-width 的 16rem 对齐 */
const SIDEBAR_WIDTH = 256;

/** 主内容在打开侧边栏后保留的最小可用宽度 */
const MIN_CONTENT_WIDTH_WITH_SIDEBAR = 400;

/** 屏幕工作区四周保留的边距，避免初始窗口贴边 */
const SCREEN_EDGE_MARGIN = 32;

/** 启动时裁剪窗口的最小高度兜底 */
const MIN_INITIAL_HEIGHT = 480;

/** 启动时按当前屏幕工作区裁剪窗口：仅当默认/上次的窗口尺寸超出屏幕可视区时才缩小 */
async function fitWindowToScreen(
  lock: () => void,
  unlock: () => void,
  onNarrow?: (logicalW: number) => void,
): Promise<void> {
  try {
    const appWindow = getCurrentWindow();
    const [monitor, scaleFactor, innerSize] = await Promise.all([
      currentMonitor(),
      appWindow.scaleFactor(),
      appWindow.innerSize(),
    ]);
    if (!monitor) return;

    const monitorW = monitor.size.width / scaleFactor;
    const monitorH = monitor.size.height / scaleFactor;
    const maxW = Math.max(MIN_CHAT_WIDTH, monitorW - SCREEN_EDGE_MARGIN * 2);
    const maxH = Math.max(MIN_INITIAL_HEIGHT, monitorH - SCREEN_EDGE_MARGIN * 2);

    let logicalW = innerSize.width / scaleFactor;
    let logicalH = innerSize.height / scaleFactor;
    let changed = false;
    if (logicalW > maxW) { logicalW = maxW; changed = true; }
    if (logicalH > maxH) { logicalH = maxH; changed = true; }
    if (!changed) return;

    lock();
    await appWindow.setSize(new LogicalSize(logicalW, logicalH));
    unlock();
    onNarrow?.(logicalW);
  } catch (error) {
    console.warn('适配窗口到屏幕失败:', error);
  }
}

/** 扩展窗口以容纳浏览器面板：对话区缩至最小宽度 + 浏览器面板宽度 */
async function expandWindowForBrowser(lock?: () => void, unlock?: () => void) {
  const appWindow = getCurrentWindow();
  const innerSize = await appWindow.innerSize();
  const scaleFactor = await appWindow.scaleFactor();
  const logicalH = innerSize.height / scaleFactor;
  const browserW = calcBrowserWidth(logicalH);
  const targetW = MIN_CHAT_WIDTH + browserW;
  lock?.();
  await appWindow.setSize(new LogicalSize(targetW, logicalH));
  unlock?.();
  return { browserW, logicalH };
}

function terminalRuntimeTabToState(tab: TerminalTabInfo): TabState {
  return {
    id: tab.id,
    kind: 'terminal',
    title: tab.title || '终端',
    url: '',
    created_at: tab.created_at || new Date().toISOString(),
    phase: tab.phase,
  };
}

export function MainApp() {
  const { applyStreamEvents, loadSessions } = useStore();
  const activeSessionId = useStore((state) => state.activeSessionId);
  useUpdateCheck();
  const [workspacePanelMounted, setWorkspacePanelMounted] = useState(false);
  const [showWorkspacePanel, setShowWorkspacePanel] = useState(false);
  const [workspaceTabKind, setWorkspaceTabKind] = useState<TabKind>('browser');
  const [workspaceOpenRequestVersion, setWorkspaceOpenRequestVersion] = useState(0);
  const [requestedTerminalTabId, setRequestedTerminalTabId] = useState<string | null>(null);
  const [terminalSyncVersion, setTerminalSyncVersion] = useState(0);
  const [chatPanelWidth, setChatPanelWidth] = useState(MIN_CHAT_WIDTH);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  // Agent 正在使用浏览器（打开/导航页面）时置位，用户点开浏览器面板后清除。
  // 用于在右上角浏览器图标上显示"使用中"标记，而不是自动弹出面板。
  const [browserAgentActive, setBrowserAgentActive] = useState(false);
  // 当前会话存在终端 tab 时置位，用户点开终端面板后清除。
  // 与浏览器标记策略一致：有 tab 即常亮。
  const [terminalAgentActive, setTerminalAgentActive] = useState(false);
  // 首次启动推荐安装的缺失默认插件列表；为 null 时不显示引导对话框。
  const [onboardingMissing, setOnboardingMissing] = useState<AvailablePlugin[] | null>(null);
  const showWorkspacePanelRef = useRef(false);
  const workspaceTabKindRef = useRef<TabKind>('browser');
  const workspaceOpenRequestIdRef = useRef(0);
  const chatPanelWidthRef = useRef(MIN_CHAT_WIDTH);
  const isDraggingRef = useRef(false);
  const streamEventQueueRef = useRef<SessionStreamEvent[]>([]);
  const streamEventTimerRef = useRef<number | null>(null);
  // sessions_updated 在一次 done 后会被连续 emit 多次（done/标题生成/Core 退役），
  // 用尾沿去抖合并刷新，并在刷新期间收到新事件时串行补跑一次。
  const sessionsRefreshTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sessionsRefreshInFlightRef = useRef(false);
  const sessionsRefreshDirtyRef = useRef(false);
  const savedWindowWidthRef = useRef<number | null>(null);
  const workspaceExpandedForBrowserRef = useRef(false);
  const preferredSidebarOpenRef = useRef(true);
  const programmaticResizeRef = useRef(false);
  const resizeLockTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const lockResize = useCallback(() => {
    programmaticResizeRef.current = true;
    if (resizeLockTimerRef.current) clearTimeout(resizeLockTimerRef.current);
  }, []);

  const unlockResize = useCallback(() => {
    resizeLockTimerRef.current = setTimeout(() => {
      programmaticResizeRef.current = false;
    }, 200);
  }, []);

  const setSidebarOpenByLayout = useCallback((open: boolean) => {
    setSidebarOpen((current) => current === open ? current : open);
  }, []);

  const handleSidebarChange = useCallback(async (open: boolean) => {
    preferredSidebarOpenRef.current = open;
    if (open) {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      const logicalW = innerSize.width / scaleFactor;
      if (logicalW <= SIDEBAR_RESTORE_THRESHOLD) {
        const logicalH = innerSize.height / scaleFactor;
        const newW = Math.max(
          logicalW + SIDEBAR_WIDTH,
          SIDEBAR_RESTORE_THRESHOLD + SIDEBAR_WIDTH,
          SIDEBAR_WIDTH + MIN_CONTENT_WIDTH_WITH_SIDEBAR,
        );
        lockResize();
        await appWindow.setSize(new LogicalSize(newW, logicalH));
        unlockResize();
      }
    }
    setSidebarOpen(open);
  }, [lockResize, unlockResize]);

  const openWorkspacePanel = useCallback(async (kind: TabKind, terminalTabId?: string | null) => {
    const requestId = workspaceOpenRequestIdRef.current + 1;
    workspaceOpenRequestIdRef.current = requestId;
    workspaceTabKindRef.current = kind;
    setWorkspaceTabKind(kind);
    setRequestedTerminalTabId(kind === 'terminal' ? terminalTabId ?? null : null);
    setWorkspaceOpenRequestVersion((version) => version + 1);
    setSidebarOpenByLayout(false);

    if (!showWorkspacePanelRef.current) {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      savedWindowWidthRef.current = innerSize.width / scaleFactor;
    }
    showWorkspacePanelRef.current = true;
    setWorkspacePanelMounted(true);
    setShowWorkspacePanel(true);

    await expandWindowForBrowser(lockResize, unlockResize);
    if (workspaceOpenRequestIdRef.current !== requestId) return;
    workspaceExpandedForBrowserRef.current = true;
    chatPanelWidthRef.current = MIN_CHAT_WIDTH;
    setChatPanelWidth(MIN_CHAT_WIDTH);

    if (kind === 'terminal') {
      await api.browserHide(useStore.getState().activeSessionId ?? useStore.getState().newConversationId ?? '').catch(console.error);
    }
  }, [lockResize, setSidebarOpenByLayout, unlockResize]);

  // 查询指定会话的浏览器 tab 数量，决定是否在浏览器图标上显示"使用中"标记。
  // 标记语义：当前会话浏览器存在 tab（即浏览器在使用中）。
  // 圆点是否可见由 StatusPanel 按 `browserAgentActive && !browserActive` 控制，
  // 因此用户已打开浏览器面板时圆点会自动隐藏，无需在此判断面板状态。
  const refreshBrowserAgentActive = useCallback(async (sessionId: string) => {
    try {
      const result = await api.browserTabList(sessionId);
      setBrowserAgentActive(result.tabs.length > 0);
    } catch {
      // 浏览器插件未就绪或会话无浏览器状态时静默
    }
  }, []);

  // 查询指定会话的终端 tab 数量，决定是否在终端图标上显示"使用中"标记。
  // 标记语义与浏览器一致：当前会话存在终端 tab 即亮标记。
  const refreshTerminalAgentActive = useCallback(async (sessionId: string) => {
    try {
      const result = await api.terminalTabList(sessionId);
      setTerminalAgentActive(result.tabs.length > 0);
    } catch {
      // 终端插件未就绪或会话无终端状态时静默
    }
  }, []);

  const closeWorkspacePanel = useCallback(async (restoreSize = true) => {
    if (!showWorkspacePanelRef.current) return;
    workspaceOpenRequestIdRef.current += 1;
    const restoreW = savedWindowWidthRef.current;
    savedWindowWidthRef.current = null;
    showWorkspacePanelRef.current = false;
    setShowWorkspacePanel(false);
    await api.browserHide(useStore.getState().activeSessionId ?? useStore.getState().newConversationId ?? '').catch(console.error);
    if (restoreSize && workspaceExpandedForBrowserRef.current) {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      const logicalH = innerSize.height / scaleFactor;
      const targetW = restoreW ?? (innerSize.width / scaleFactor - calcBrowserWidth(logicalH));
      lockResize();
      await appWindow.setSize(new LogicalSize(targetW, logicalH));
      unlockResize();
      if (targetW > SIDEBAR_RESTORE_THRESHOLD && preferredSidebarOpenRef.current) {
        setSidebarOpenByLayout(true);
      }
    } else if (preferredSidebarOpenRef.current) {
      setSidebarOpenByLayout(true);
    }
    workspaceExpandedForBrowserRef.current = false;
    // 面板关闭后浏览器/终端 tab 仍然存活（仅隐藏），刷新"使用中"标记让圆点重新亮起
    const sessionId = useStore.getState().activeSessionId ?? useStore.getState().newConversationId;
    if (sessionId) {
      void refreshBrowserAgentActive(sessionId);
      void refreshTerminalAgentActive(sessionId);
    }
  }, [lockResize, refreshBrowserAgentActive, refreshTerminalAgentActive, setSidebarOpenByLayout, unlockResize]);

  // 浏览器面板挂载后，显式触达后端以渲染浏览器表面。
  // 与 `browser:open` / `tiangong:open-browser` 入口保持一致，
  // 避免依赖 TabsContainer 的隐式激活 effect（首次挂载时被 hydration 短路）。

  const handleToggleBrowser = useCallback(async () => {
    // 标题栏按钮只表达"打开 browser 意图"——Tab 的查找/切换/创建统一由
    // TabsContainer.activateOrCreateTab 执行（不再调 ensureBrowserVisible）
    // 用户主动查看浏览器面板后，清除"agent 使用中"标记
    setBrowserAgentActive(false);
    await openWorkspacePanel('browser');
  }, [openWorkspacePanel]);

  const handleToggleTerminal = useCallback(() => {
    // 用户主动查看终端面板后，清除"使用中"标记
    setTerminalAgentActive(false);
    void openWorkspacePanel('terminal');
  }, [openWorkspacePanel]);

  const handleWorkspaceActiveKindChange = useCallback((kind: TabKind | null) => {
    if (!kind) return;
    workspaceTabKindRef.current = kind;
    setWorkspaceTabKind(kind);
  }, []);

  const syncTerminalRuntimeTabsToSession = useCallback(async (
    sessionId: string,
    preferredTabId?: string | null,
  ): Promise<boolean> => {
    const [sessionTabs, runtimeTabs] = await Promise.all([
      api.getSessionTabs(sessionId),
      api.terminalTabList(sessionId),
    ]);
    if (runtimeTabs.tabs.length === 0) return false;

    const terminalTabs = runtimeTabs.tabs.map(terminalRuntimeTabToState);
    const terminalById = new Map(terminalTabs.map((tab) => [tab.id, tab]));
    const nextExistingTabs = sessionTabs.tabs
      .filter((tab) => tab.kind !== 'terminal' || terminalById.has(tab.id))
      .map((tab) => (
        tab.kind === 'terminal' && terminalById.has(tab.id)
          ? terminalById.get(tab.id)!
          : tab
      ));
    const existingIds = new Set(nextExistingTabs.map((tab) => tab.id));
    const nextTabs = [
      ...nextExistingTabs,
      ...terminalTabs.filter((tab) => !existingIds.has(tab.id)),
    ];
    const nextActiveTabId = [
      preferredTabId,
      runtimeTabs.active_tab_id,
      sessionTabs.active_tab_id,
      nextTabs[0]?.id,
    ].find((tabId) => tabId && nextTabs.some((tab) => tab.id === tabId)) || null;

    await api.setSessionTabs(sessionId, nextTabs, nextActiveTabId);
    return true;
  }, []);

  const handleDividerDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isDraggingRef.current = true;
    const mainEl = document.querySelector('main');
    if (!mainEl) return;

    const cleanup = () => {
      isDraggingRef.current = false;
      document.removeEventListener('mousemove', onMove);
      document.removeEventListener('mouseup', onUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    const onUp = () => { cleanup(); };

    const onMove = (ev: MouseEvent) => {
      if (!isDraggingRef.current) return;
      const rect = mainEl.getBoundingClientRect();
      const next = ev.clientX - rect.left;
      // 拖到右侧剩余宽度小于面板最小宽度时，关闭工作区面板。
      if (rect.width - next < MIN_BROWSER_WIDTH) {
        cleanup();
        void closeWorkspacePanel(false);
        return;
      }
      const clamped = Math.max(MIN_CHAT_WIDTH, next);
      setChatPanelWidth(clamped);
      chatPanelWidthRef.current = clamped;
    };

    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', onUp);
  }, [closeWorkspacePanel]);

  // 会话切换时刷新浏览器"使用中"标记：新切到的会话可能已有浏览器 tab。
  useEffect(() => {
    if (!activeSessionId) {
      setBrowserAgentActive(false);
      setTerminalAgentActive(false);
      return;
    }
    void refreshBrowserAgentActive(activeSessionId);
    void refreshTerminalAgentActive(activeSessionId);
  }, [activeSessionId, refreshBrowserAgentActive, refreshTerminalAgentActive]);

  useEffect(() => {
    ensureDesktopNotificationPermission().catch(console.warn);

    // 启动时按当前屏幕工作区裁剪初始窗口，避免低分辨率屏幕上窗口超出可视区
    fitWindowToScreen(lockResize, unlockResize, (logicalW) => {
      if (logicalW <= SIDEBAR_RESTORE_THRESHOLD) {
        setSidebarOpenByLayout(false);
      }
    }).catch(console.warn);

    loadSessions();

    // 首次启动时检测缺失的默认插件，有缺失则弹出推荐安装引导。
    // 独立 catch，网络异常或检测失败都不影响主初始化流程。
    api.checkDefaultPlugins()
      .then((check) => {
        if (check.first_launch_pending && check.missing.length > 0) {
          setOnboardingMissing(check.missing);
        }
      })
      .catch((error) => console.warn('默认插件检测失败', error));

    const flushStreamEvents = () => {
      streamEventTimerRef.current = null;
      const events = streamEventQueueRef.current;
      streamEventQueueRef.current = [];
      applyStreamEvents(events);
    };

    const scheduleStreamEvent = (event: SessionStreamEvent) => {
      streamEventQueueRef.current.push(event);
      if (streamEventTimerRef.current !== null) return;
      streamEventTimerRef.current = window.setTimeout(flushStreamEvents, 16);
    };

    // 本轮 effect 的生命周期标记与取消函数集合。
    // 关键：不使用跨 effect 的单值引用（会被旧异步流程覆盖，导致第一轮监听器成为孤儿）。
    // 每个 await 注册完成后立即交给 track 收纳；guard 在每个 await 点之后检查 disposed，
    // 已清理则立即取消并中止后续注册，避免 StrictMode 挂载→清理→再挂载时第一轮监听器残留。
    let disposed = false;
    const cleanups: UnlistenFn[] = [];
    const track = (un: UnlistenFn) => {
      if (disposed) {
        un();
        return;
      }
      cleanups.push(un);
    };
    const guard = () => {
      if (disposed) throw new DOMException('Aborted', 'AbortError');
    };

    const setupListener = async () => {
      try {
        track(await api.onStreamEvent(scheduleStreamEvent));
        guard();

        const { listen } = await import('@tauri-apps/api/event');
      // 尾沿去抖 + in-flight 串行化 + dirty rerun。
      // - 每次事件都重置 120ms 计时器，等事件静默后再刷新；
      // - 刷新进行中来的事件标记 dirty，settle 后 rerun 一次，避免并发读取。
      // 刷新失败时由 store 保留旧列表，后续事件会再次触发收敛。
      const runProtectiveRefresh = async () => {
        if (sessionsRefreshInFlightRef.current) {
          sessionsRefreshDirtyRef.current = true;
          return;
        }
        sessionsRefreshInFlightRef.current = true;
        sessionsRefreshDirtyRef.current = false;
        try {
          await loadSessions({ protective: true });
        } finally {
          sessionsRefreshInFlightRef.current = false;
          if (sessionsRefreshDirtyRef.current) {
            sessionsRefreshDirtyRef.current = false;
            runProtectiveRefresh();
          }
        }
      };
      const scheduleSessionsRefresh = () => {
        if (sessionsRefreshTimerRef.current !== null) {
          clearTimeout(sessionsRefreshTimerRef.current);
        }
        sessionsRefreshTimerRef.current = setTimeout(() => {
          sessionsRefreshTimerRef.current = null;
          runProtectiveRefresh();
        }, 120);
      };
      track(await listen('sessions_updated', () => {
        scheduleSessionsRefresh();
      }));
      guard();
      track(await listen<string>('desktop_notification_open_session', (event) => {
        const sessionId = event.payload;
        if (sessionId) {
          useStore.getState().switchSession(sessionId).catch(console.error);
        }
      }));
      guard();

      // 浏览器 tab 增删/更新时刷新"使用中"标记。
      // 标记语义：当前会话浏览器存在 tab（即浏览器在使用），且用户尚未打开浏览器面板。
      track(await listen<{ session_id?: string }>('browser:tab_updated', (event) => {
        const sessionId = event.payload?.session_id;
        if (!sessionId || useStore.getState().activeSessionId !== sessionId) return;
        void refreshBrowserAgentActive(sessionId);
      }));
      guard();
      // 保留 browser:open 监听：后端首次初始化浏览器窗口时触发，同样刷新标记。
      track(await listen<{ session_id: string; url: string }>('browser:open', (event) => {
        const { session_id } = event.payload;
        if (!session_id || useStore.getState().activeSessionId !== session_id) return;
        void refreshBrowserAgentActive(session_id);
      }));
      guard();
      // agent_active 信号：agent 打开/导航页面时发出，刷新标记。
      track(await listen<{ session_id: string }>('browser:agent_active', (event) => {
        const { session_id } = event.payload;
        if (!session_id || useStore.getState().activeSessionId !== session_id) return;
        void refreshBrowserAgentActive(session_id);
      }));
      guard();
      track(await listen<{
        session_id: string;
        active_tab_id?: string | null;
        source?: string | null;
      }>('terminal:tab_updated', async (event) => {
        const { session_id, active_tab_id, source } = event.payload;
        const store = useStore.getState();
        const terminalSessionId = store.activeSessionId || store.newConversationId;
        const isCurrentTerminalSession = Boolean(terminalSessionId && session_id === terminalSessionId);
        const isNewConversationTerminalSession = Boolean(store.newConversationId && session_id === store.newConversationId);
        let synced = false;
        if (!isNewConversationTerminalSession) {
          synced = await syncTerminalRuntimeTabsToSession(session_id, active_tab_id ?? null)
            .catch((error) => {
              console.error('同步终端 Tab 到会话失败：', error);
              return false;
            });
        }
        if (synced && isCurrentTerminalSession) {
          setWorkspacePanelMounted(true);
          setTerminalSyncVersion((version) => version + 1);
        }
        // 当前会话的终端 tab 发生变化时刷新"使用中"标记
        if (isCurrentTerminalSession) {
          void refreshTerminalAgentActive(session_id);
        }
        if (!isCurrentTerminalSession) {
          return;
        }
        if (source === 'restore' || source === 'agent_command') {
          return;
        }
        if (showWorkspacePanelRef.current && workspaceTabKindRef.current === 'terminal' && active_tab_id) {
          await openWorkspacePanel('terminal', active_tab_id);
        }
      }));
      guard();

      track(await getCurrentWindow().onResized(async () => {
        if (programmaticResizeRef.current) return;

        const appWindow = getCurrentWindow();
        const innerSize = await appWindow.innerSize();
        const scaleFactor = await appWindow.scaleFactor();
        const logicalW = innerSize.width / scaleFactor;

        if (showWorkspacePanelRef.current) {
          const mainEl = document.querySelector('main');
          const sidebarW = mainEl ? mainEl.offsetLeft : 0;
          const browserSpace = logicalW - sidebarW - chatPanelWidthRef.current;
          if (browserSpace < MIN_BROWSER_WIDTH) {
            await closeWorkspacePanel(false);
          }
        }

        if (logicalW <= SIDEBAR_RESTORE_THRESHOLD) {
          setSidebarOpenByLayout(false);
        } else if (
          !showWorkspacePanelRef.current
          && preferredSidebarOpenRef.current
        ) {
          setSidebarOpenByLayout(true);
        }
      }));
      } catch (error) {
        // effect 已被清理（guard 抛 AbortError），释放本轮已收纳的监听。
        if ((error as DOMException)?.name !== 'AbortError') {
          throw error;
        }
      }
    };

    setupListener();

    api.getWorkspaceDir()
      .then((workspaceDir) => {
        useStore.setState((state) => ({
          workspaceDir,
          sessionCwd: state.activeSessionId ? state.sessionCwd : state.sessionCwd || workspaceDir,
        }));
      })
      .catch(console.error);

    const onOpenBrowser = async (e: Event) => {
      const url = (e as CustomEvent).detail;
      if (typeof url !== 'string' || !url.trim()) return;
      const store = useStore.getState();
      const sessionId = store.activeSessionId || store.newConversationId;
      if (!sessionId) {
        console.error('无法打开浏览器：缺少 session_id');
        return;
      }
      // 后端原子完成导航（避免面板 hydrate 与导航竞争），再打开面板
      await api.browserOpenUrl(sessionId, url).catch(error =>
        console.error('打开浏览器地址失败:', error)
      );
      await openWorkspacePanel('browser');
    };
    window.addEventListener('tiangong:open-browser', onOpenBrowser);

    return () => {
      // 先标记本轮 disposed，使尚未完成的异步注册流程在后续 guard 处自行放弃；
      // 再释放本轮已收纳的监听，避免遗留孤儿监听导致事件被重复消费。
      disposed = true;
      while (cleanups.length > 0) {
        const un = cleanups.pop();
        un?.();
      }
      if (streamEventTimerRef.current !== null) {
        window.clearTimeout(streamEventTimerRef.current);
        streamEventTimerRef.current = null;
      }
      streamEventQueueRef.current = [];
      if (sessionsRefreshTimerRef.current !== null) {
        clearTimeout(sessionsRefreshTimerRef.current);
        sessionsRefreshTimerRef.current = null;
      }
      window.removeEventListener('tiangong:open-browser', onOpenBrowser);
    };
  }, [
    setSidebarOpenByLayout,
    applyStreamEvents,
    openWorkspacePanel,
    closeWorkspacePanel,
    lockResize,
    unlockResize,
    refreshBrowserAgentActive,
    refreshTerminalAgentActive,
    syncTerminalRuntimeTabsToSession,
  ]);

  return (
    <SidebarProvider open={sidebarOpen} onOpenChange={handleSidebarChange}>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        <LazyStatusPanel
          browserActive={showWorkspacePanel && workspaceTabKind === 'browser'}
          browserAgentActive={browserAgentActive}
          onOpenBrowser={handleToggleBrowser}
          terminalActive={showWorkspacePanel && workspaceTabKind === 'terminal'}
          terminalAgentActive={terminalAgentActive}
          onOpenTerminal={handleToggleTerminal}
        />

        <div className="flex flex-1 min-h-0">
          <AppSidebar />

          <main className="flex flex-1 flex-col min-w-0 bg-background">
            <div className="flex flex-1 min-h-0">
              <div
                className={`flex flex-col min-w-0 ${showWorkspacePanel ? 'shrink-0' : 'flex-1'}`}
                style={showWorkspacePanel ? { width: chatPanelWidth } : undefined}
              >
                <div className="flex-1 overflow-hidden">
                  <LazyMessageList />
                </div>

                <LazyMessageInput />
              </div>

              {workspacePanelMounted && showWorkspacePanel && (
                <div
                  className="w-[3px] shrink-0 cursor-col-resize bg-border hover:bg-muted-foreground/30 active:bg-muted-foreground/50 transition-colors"
                  onMouseDown={handleDividerDrag}
                />
              )}

              {workspacePanelMounted && (
                <div className={`min-w-0 flex-1 ${showWorkspacePanel ? 'flex' : 'hidden'}`}>
                  <TabsContainer
                    initialTabKind={workspaceTabKind}
                    isVisible={showWorkspacePanel}
                    openRequestVersion={workspaceOpenRequestVersion}
                    requestedTerminalTabId={requestedTerminalTabId}
                    terminalSyncVersion={terminalSyncVersion}
                    onClose={() => { void closeWorkspacePanel(); }}
                    onActiveKindChange={handleWorkspaceActiveKindChange}
                  />
                </div>
              )}
            </div>
          </main>
        </div>
      </div>

      <DefaultPluginOnboarding
        missing={onboardingMissing}
        onOpenChange={(open) => {
          if (!open) setOnboardingMissing(null);
        }}
        onComplete={() => {
          /* 安装后 registry 会热加载，Core 创建时按需感知已装插件，无需额外刷新 */
        }}
      />
    </SidebarProvider>
  );
}
