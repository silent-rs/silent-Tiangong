import { useEffect, useRef, useState, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { api, type RunSnapshot } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { BrowserPanel } from '@/components/BrowserPanel';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';

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

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const [showBrowser, setShowBrowser] = useState(false);
  const [chatPanelWidth, setChatPanelWidth] = useState(MIN_CHAT_WIDTH);
  const [browserUrl, setBrowserUrl] = useState<string | undefined>(undefined);
  const [navigateUrl, setNavigateUrl] = useState<string | undefined>(undefined);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const showBrowserRef = useRef(false);
  const chatPanelWidthRef = useRef(MIN_CHAT_WIDTH);
  const isDraggingRef = useRef(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const latestSnapshotRef = useRef<RunSnapshot | null>(null);
  const snapshotTimerRef = useRef<number | null>(null);
  const savedWindowWidthRef = useRef<number | null>(null);
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

  const openBrowserPanel = useCallback(async () => {
    const appWindow = getCurrentWindow();
    const innerSize = await appWindow.innerSize();
    const scaleFactor = await appWindow.scaleFactor();
    savedWindowWidthRef.current = innerSize.width / scaleFactor;
    setSidebarOpenByLayout(false);
    await expandWindowForBrowser(lockResize, unlockResize);
    chatPanelWidthRef.current = MIN_CHAT_WIDTH;
    setChatPanelWidth(MIN_CHAT_WIDTH);
    showBrowserRef.current = true;
    setShowBrowser(true);
  }, [lockResize, setSidebarOpenByLayout, unlockResize]);

  const handleToggleBrowser = useCallback(async () => {
    if (!showBrowserRef.current) {
      await openBrowserPanel();
    } else {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      const logicalH = innerSize.height / scaleFactor;
      const restoreW = savedWindowWidthRef.current ?? (innerSize.width / scaleFactor - calcBrowserWidth(logicalH));
      savedWindowWidthRef.current = null;
      showBrowserRef.current = false;
      setShowBrowser(false);
      await api.browserHide().catch(console.error);
      lockResize();
      await appWindow.setSize(new LogicalSize(restoreW, logicalH));
      unlockResize();
      if (restoreW > SIDEBAR_RESTORE_THRESHOLD && preferredSidebarOpenRef.current) {
        setSidebarOpenByLayout(true);
      }
    }
  }, [openBrowserPanel, lockResize, setSidebarOpenByLayout, unlockResize]);

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
      if (rect.width - next < MIN_BROWSER_WIDTH) {
        cleanup();
        api.browserHide().catch(console.error);
        savedWindowWidthRef.current = null;
        showBrowserRef.current = false;
        setShowBrowser(false);
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
  }, []);

  useEffect(() => {
    ensureDesktopNotificationPermission().catch(console.warn);

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

      const unlistenBrowserOpen = await listen<string>('browser:open', async (event) => {
        const url = event.payload;
        setBrowserUrl(url);
        setNavigateUrl(url);
        if (!showBrowserRef.current) {
          await openBrowserPanel();
        }
      });

      const unlistenBrowserPageLoaded = await listen<{ title: string; url: string; text: string }>('browser:page_loaded', (event) => {
        setBrowserUrl(event.payload.url);
      });

      const unlistenResize = await getCurrentWindow().onResized(async () => {
        if (programmaticResizeRef.current) return;

        const appWindow = getCurrentWindow();
        const innerSize = await appWindow.innerSize();
        const scaleFactor = await appWindow.scaleFactor();
        const logicalW = innerSize.width / scaleFactor;

        if (showBrowserRef.current) {
          const mainEl = document.querySelector('main');
          const sidebarW = mainEl ? mainEl.offsetLeft : 0;
          const browserSpace = logicalW - sidebarW - chatPanelWidthRef.current;
          if (browserSpace < MIN_BROWSER_WIDTH) {
            await api.browserHide().catch(console.error);
            savedWindowWidthRef.current = null;
            showBrowserRef.current = false;
            setShowBrowser(false);
          }
        }

        if (logicalW <= SIDEBAR_RESTORE_THRESHOLD) {
          setSidebarOpenByLayout(false);
        } else if (
          !showBrowserRef.current
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
        unlistenBrowserPageLoaded();
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
      if (typeof url === 'string') {
        setBrowserUrl(url);
        setNavigateUrl(url);
        if (!showBrowserRef.current) {
          await openBrowserPanel();
        }
      }
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
  }, [setSidebarOpenByLayout, openBrowserPanel]);

  return (
    <SidebarProvider open={sidebarOpen} onOpenChange={handleSidebarChange}>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        <LazyStatusPanel
          showBrowser={showBrowser}
          onToggleBrowser={handleToggleBrowser}
        />

        <div className="flex flex-1 min-h-0">
          <AppSidebar />

          <main className="flex flex-1 flex-col min-w-0 bg-background">
            <div className="flex flex-1 min-h-0">
              <div
                className={`flex flex-col min-w-0 ${showBrowser ? 'shrink-0' : 'flex-1'}`}
                style={showBrowser ? { width: chatPanelWidth } : undefined}
              >
                <div className="flex-1 overflow-hidden">
                  <LazyMessageList />
                </div>

                <LazyMessageInput />
              </div>

              {showBrowser && (
                <div
                  className="w-[3px] shrink-0 cursor-col-resize bg-border hover:bg-muted-foreground/30 active:bg-muted-foreground/50 transition-colors"
                  onMouseDown={handleDividerDrag}
                />
              )}

              {showBrowser && (
                <BrowserPanel initialUrl={browserUrl} currentUrl={browserUrl} navigateUrl={navigateUrl} />
              )}
            </div>
          </main>
        </div>
      </div>
    </SidebarProvider>
  );
}
