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

/** 扩展窗口以容纳浏览器面板，精确计算：窗口宽度 = sidebar + 400(对话) + browserW */
async function expandWindowForBrowser() {
  const appWindow = getCurrentWindow();
  const innerSize = await appWindow.innerSize();
  const scaleFactor = await appWindow.scaleFactor();
  const logicalW = innerSize.width / scaleFactor;
  const logicalH = innerSize.height / scaleFactor;
  const browserW = calcBrowserWidth(logicalH);
  // 当前对话宽度（flex-1 占满 main 区域）
  const mainEl = document.querySelector('main');
  const chatW = mainEl?.clientWidth ?? 400;
  // 扩展量 = 浏览器宽度 - 对话从 flex-1 收缩到 400 的差值
  const expand = browserW - (chatW - 400);
  const newW = logicalW + expand;
  await appWindow.setSize(new LogicalSize(newW, logicalH));
  return { browserW, logicalW: logicalW, logicalH };
}

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const [showBrowser, setShowBrowser] = useState(false);
  const [browserUrl, setBrowserUrl] = useState<string | undefined>(undefined);
  const showBrowserRef = useRef(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const latestSnapshotRef = useRef<RunSnapshot | null>(null);
  const snapshotTimerRef = useRef<number | null>(null);
  const savedWindowWidthRef = useRef<number | null>(null);

  const handleToggleBrowser = useCallback(async () => {
    if (!showBrowserRef.current) {
      // 保存原始窗口宽度
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      savedWindowWidthRef.current = innerSize.width / scaleFactor;
      // 扩展窗口
      await expandWindowForBrowser();
      showBrowserRef.current = true;
      setShowBrowser(true);
    } else {
      await api.browserHide().catch(console.error);
      // 恢复原始窗口宽度
      const appWindow = getCurrentWindow();
      const innerSize = await appWindow.innerSize();
      const scaleFactor = await appWindow.scaleFactor();
      const logicalH = innerSize.height / scaleFactor;
      const restoreW = savedWindowWidthRef.current ?? (innerSize.width / scaleFactor - calcBrowserWidth(logicalH));
      savedWindowWidthRef.current = null;
      await appWindow.setSize(new LogicalSize(restoreW, logicalH));
      showBrowserRef.current = false;
      setShowBrowser(false);
    }
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

      // 监听 sessions 列表更新（标题变化等）
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

      // 监听浏览器自动打开事件
      const unlistenBrowserOpen = await listen<string>('browser:open', async (event) => {
        const url = event.payload;
        setBrowserUrl(url);
        if (!showBrowserRef.current) {
          const appWindow = getCurrentWindow();
          const innerSize = await appWindow.innerSize();
          const scaleFactor = await appWindow.scaleFactor();
          savedWindowWidthRef.current = innerSize.width / scaleFactor;
          await expandWindowForBrowser();
          showBrowserRef.current = true;
          setShowBrowser(true);
        }
      });

      // 监听浏览器页面加载完成事件
      const unlistenBrowserPageLoaded = await listen<{ title: string; url: string; text: string }>('browser:page_loaded', (event) => {
        setBrowserUrl(event.payload.url);
      });

      const prevUnlisten = unlistenRef.current;
      unlistenRef.current = () => {
        prevUnlisten?.();
        unlistenSessions();
        unlistenOpenSession();
        unlistenBrowserOpen();
        unlistenBrowserPageLoaded();
      };
    };

    setupListener();

    // 加载初始工作空间和当前对话目录
    Promise.all([api.getWorkspaceDir(), api.getSessionCwd()])
      .then(([workspaceDir, sessionCwd]) => {
        useStore.setState({ workspaceDir, sessionCwd });
      })
      .catch(console.error);

    // 监听来自消息列表的链接点击，在嵌入浏览器中打开
    const onOpenBrowser = async (e: Event) => {
      const url = (e as CustomEvent).detail;
      if (typeof url === 'string') {
        setBrowserUrl(url);
        if (!showBrowserRef.current) {
          const appWindow = getCurrentWindow();
          const innerSize = await appWindow.innerSize();
          const scaleFactor = await appWindow.scaleFactor();
          savedWindowWidthRef.current = innerSize.width / scaleFactor;
          await expandWindowForBrowser();
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
  }, []);

  return (
    <SidebarProvider>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        {/* 顶部 Header — 横跨全宽，固定在最顶部 */}
        <LazyStatusPanel
          showBrowser={showBrowser}
          onToggleBrowser={handleToggleBrowser}
        />

        {/* 下方区域：Sidebar + 主内容 */}
        <div className="flex flex-1 min-h-0">
          <AppSidebar />

          {/* 主内容区 */}
          <main className="flex flex-1 flex-col min-w-0 bg-background">
            <div className="flex flex-1 min-h-0">
              <div
                className={`flex flex-col min-w-0 ${showBrowser ? 'shrink-0' : 'flex-1'}`}
                style={showBrowser ? { width: 400 } : undefined}
              >
                {/* 消息列表 */}
                <div className="flex-1 overflow-hidden">
                  <LazyMessageList />
                </div>

                {/* 输入框 */}
                <LazyMessageInput />
              </div>

              {/* 浏览器面板 */}
              {showBrowser && (
                <div className="flex-1 flex justify-center overflow-hidden">
                  <BrowserPanel initialUrl={browserUrl} currentUrl={browserUrl} />
                </div>
              )}
            </div>
          </main>
        </div>
      </div>
    </SidebarProvider>
  );
}
