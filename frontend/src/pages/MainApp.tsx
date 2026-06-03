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

/** 侧边栏自动隐藏/恢复阈值 */
const SIDEBAR_RESTORE_THRESHOLD = 656;

/** 侧边栏宽度：与 CSS 变量 --sidebar-width 的 16rem 对齐 */
const SIDEBAR_WIDTH = 256;

/** 主内容在打开侧边栏后保留的最小可用宽度 */
const MIN_CONTENT_WIDTH_WITH_SIDEBAR = 400;

/** 扩展窗口以容纳浏览器面板，精确计算：窗口宽度 = sidebar + 400(对话) + browserW */
async function expandWindowForBrowser(lock?: () => void, unlock?: () => void) {
  const appWindow = getCurrentWindow();
  const innerSize = await appWindow.innerSize();
  const scaleFactor = await appWindow.scaleFactor();
  const logicalW = innerSize.width / scaleFactor;
  const logicalH = innerSize.height / scaleFactor;
  const browserW = calcBrowserWidth(logicalH);
  const mainEl = document.querySelector('main');
  const chatW = mainEl?.clientWidth ?? 400;
  const expand = browserW - (chatW - 400);
  const newW = logicalW + expand;
  lock?.();
  await appWindow.setSize(new LogicalSize(newW, logicalH));
  unlock?.();
  return { browserW, logicalW: logicalW, logicalH };
}

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const [showBrowser, setShowBrowser] = useState(false);
  const [browserUrl, setBrowserUrl] = useState<string | undefined>(undefined);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const showBrowserRef = useRef(false);
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

  const handleToggleBrowser = useCallback(async () => {
    if (!showBrowserRef.current) {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      savedWindowWidthRef.current = innerSize.width / scaleFactor;
      await expandWindowForBrowser(lockResize, unlockResize);
      showBrowserRef.current = true;
      setShowBrowser(true);
    } else {
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      const logicalH = innerSize.height / scaleFactor;
      const restoreW = savedWindowWidthRef.current ?? (innerSize.width / scaleFactor - calcBrowserWidth(logicalH));
      savedWindowWidthRef.current = null;
      // 先卸载 BrowserPanel（断开 ResizeObserver），再隐藏 WebView 和收缩窗口
      // 避免窗口收缩时 ResizeObserver 的 syncPosition 把 WebView 拉回可见区域
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
  }, [lockResize, setSidebarOpenByLayout, unlockResize]);

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
        if (!showBrowserRef.current) {
          const appWindow = getCurrentWindow();
          const innerSize = await appWindow.innerSize();
          const scaleFactor = await appWindow.scaleFactor();
          savedWindowWidthRef.current = innerSize.width / scaleFactor;
          await expandWindowForBrowser(lockResize, unlockResize);
          showBrowserRef.current = true;
          setShowBrowser(true);
        }
      });

      const unlistenBrowserPageLoaded = await listen<{ title: string; url: string; text: string }>('browser:page_loaded', (event) => {
        setBrowserUrl(event.payload.url);
      });

      // 窗口大小变化：浏览器/侧边栏自适应（仅用户手动拖拽时触发）
      const unlistenResize = await getCurrentWindow().onResized(async () => {
        if (programmaticResizeRef.current) return;

        const appWindow = getCurrentWindow();
        const innerSize = await appWindow.innerSize();
        const scaleFactor = await appWindow.scaleFactor();
        const logicalW = innerSize.width / scaleFactor;

        // 浏览器打开时宽度不足则隐藏
        if (showBrowserRef.current) {
          const mainEl = document.querySelector('main');
          const sidebarW = mainEl ? mainEl.offsetLeft : 0;
          const browserSpace = logicalW - sidebarW - 400;
          if (browserSpace < MIN_BROWSER_WIDTH) {
            await api.browserHide().catch(console.error);
            savedWindowWidthRef.current = null;
            showBrowserRef.current = false;
            setShowBrowser(false);
          }
        }

        // 窗口宽度不足时临时隐藏侧边栏，宽度恢复后再按用户原状态恢复
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
        if (!showBrowserRef.current) {
          const appWindow = getCurrentWindow();
          const innerSize = await appWindow.innerSize();
          const scaleFactor = await appWindow.scaleFactor();
          savedWindowWidthRef.current = innerSize.width / scaleFactor;
          await expandWindowForBrowser(lockResize, unlockResize);
          showBrowserRef.current = true;
          setShowBrowser(true);
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
  }, [setSidebarOpenByLayout]);

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
                style={showBrowser ? { width: 400 } : undefined}
              >
                <div className="flex-1 overflow-hidden">
                  <LazyMessageList />
                </div>

                <LazyMessageInput />
              </div>

              {showBrowser && (
                <BrowserPanel initialUrl={browserUrl} currentUrl={browserUrl} />
              )}
            </div>
          </main>
        </div>
      </div>
    </SidebarProvider>
  );
}
