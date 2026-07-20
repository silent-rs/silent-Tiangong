import { useEffect, useRef, useState, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { api, type RunSnapshot, type TabKind, type TabState, type TerminalTabInfo } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
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
  };
}

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  useUpdateCheck();
  const [workspacePanelMounted, setWorkspacePanelMounted] = useState(false);
  const [showWorkspacePanel, setShowWorkspacePanel] = useState(false);
  const [workspaceTabKind, setWorkspaceTabKind] = useState<TabKind>('browser');
  const [workspaceOpenRequestVersion, setWorkspaceOpenRequestVersion] = useState(0);
  const [requestedTerminalTabId, setRequestedTerminalTabId] = useState<string | null>(null);
  const [terminalSyncVersion, setTerminalSyncVersion] = useState(0);
  const [chatPanelWidth, setChatPanelWidth] = useState(MIN_CHAT_WIDTH);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const showWorkspacePanelRef = useRef(false);
  const workspaceTabKindRef = useRef<TabKind>('browser');
  const workspaceOpenRequestIdRef = useRef(0);
  const chatPanelWidthRef = useRef(MIN_CHAT_WIDTH);
  const isDraggingRef = useRef(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const latestSnapshotRef = useRef<RunSnapshot | null>(null);
  const snapshotTimerRef = useRef<number | null>(null);
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
  }, [lockResize, setSidebarOpenByLayout, unlockResize]);

  // 浏览器面板挂载后，显式触达后端以渲染浏览器表面。
  // 与 `browser:open` / `tiangong:open-browser` 入口保持一致，
  // 避免依赖 TabsContainer 的隐式激活 effect（首次挂载时被 hydration 短路）。

  const handleToggleBrowser = useCallback(async () => {
    // 标题栏按钮只表达"打开 browser 意图"——Tab 的查找/切换/创建统一由
    // TabsContainer.activateOrCreateTab 执行（不再调 ensureBrowserVisible）
    await openWorkspacePanel('browser');
  }, [openWorkspacePanel]);

  const handleToggleTerminal = useCallback(() => {
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

  useEffect(() => {
    ensureDesktopNotificationPermission().catch(console.warn);

    // 启动时按当前屏幕工作区裁剪初始窗口，避免低分辨率屏幕上窗口超出可视区
    fitWindowToScreen(lockResize, unlockResize, (logicalW) => {
      if (logicalW <= SIDEBAR_RESTORE_THRESHOLD) {
        setSidebarOpenByLayout(false);
      }
    }).catch(console.warn);

    loadSessions();

    const flushSnapshot = () => {
      snapshotTimerRef.current = null;
      const snapshot = latestSnapshotRef.current;
      latestSnapshotRef.current = null;
      if (snapshot) {
        updateFromSnapshot(snapshot);
      }
    };

    const scheduleSnapshotUpdate = (snapshot: RunSnapshot) => {
      latestSnapshotRef.current = snapshot;
      if (snapshotTimerRef.current !== null) {
        return;
      }
      snapshotTimerRef.current = window.setTimeout(flushSnapshot, 16);
    };

    const setupListener = async () => {
      const unlisten = await api.onRunSnapshot((snapshot) => {
        scheduleSnapshotUpdate(snapshot);
      });
      unlistenRef.current = unlisten;

      const { listen } = await import('@tauri-apps/api/event');
      const unlistenSessions = await listen('sessions_updated', () => {
        loadSessions();
      });
      const unlistenOpenSession = await listen<string>('desktop_notification_open_session', (event) => {
        const sessionId = event.payload;
        if (sessionId) {
          useStore.getState().switchSession(sessionId).catch(console.error);
        }
      });

      const unlistenBrowserOpen = await listen<{ session_id: string; url: string }>('browser:open', async (event) => {
        const { session_id } = event.payload;
        if (!session_id || useStore.getState().activeSessionId !== session_id) return;
        await openWorkspacePanel('browser');
        await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
        // browser:open 已在后端完成导航，前端不再重复 navigate
      });
      const unlistenTerminalTabUpdated = await listen<{
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
        if (!isCurrentTerminalSession) {
          return;
        }
        if (source === 'restore' || source === 'agent_command') {
          return;
        }
        if (showWorkspacePanelRef.current && workspaceTabKindRef.current === 'terminal' && active_tab_id) {
          await openWorkspacePanel('terminal', active_tab_id);
        }
      });

      const unlistenResize = await getCurrentWindow().onResized(async () => {
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
      });

      const prevUnlisten = unlistenRef.current;
      unlistenRef.current = () => {
        prevUnlisten?.();
        unlistenSessions();
        unlistenOpenSession();
        unlistenBrowserOpen();
        unlistenTerminalTabUpdated();
        unlistenResize();
      };
    };

    setupListener();

    Promise.all([api.getWorkspaceDir(), api.getSessionCwd()])
      .then(([workspaceDir, sessionCwd]) => {
        useStore.setState({ workspaceDir, sessionCwd });
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
      if (snapshotTimerRef.current !== null) {
        window.clearTimeout(snapshotTimerRef.current);
        snapshotTimerRef.current = null;
      }
      window.removeEventListener('tiangong:open-browser', onOpenBrowser);
      unlistenRef.current?.();
    };
  }, [
    setSidebarOpenByLayout,
    openWorkspacePanel,
    closeWorkspacePanel,
    lockResize,
    unlockResize,
    syncTerminalRuntimeTabsToSession,
  ]);

  return (
    <SidebarProvider open={sidebarOpen} onOpenChange={handleSidebarChange}>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        <LazyStatusPanel
          browserActive={showWorkspacePanel && workspaceTabKind === 'browser'}
          onOpenBrowser={handleToggleBrowser}
          terminalActive={showWorkspacePanel && workspaceTabKind === 'terminal'}
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
    </SidebarProvider>
  );
}
