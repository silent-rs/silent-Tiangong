import { useEffect, useRef, useState, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { api, type RunSnapshot } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { BrowserPanel } from '@/components/BrowserPanel';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import type { UnlistenFn } from '@tauri-apps/api/event';

const BROWSER_MIN_WIDTH = 320;
const BROWSER_DEFAULT_WIDTH = 500;
const BROWSER_MAX_WIDTH_RATIO = 0.65;

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const [showBrowser, setShowBrowser] = useState(false);
  const [browserWidth, setBrowserWidth] = useState(BROWSER_DEFAULT_WIDTH);
  const draggingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(0);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const latestSnapshotRef = useRef<RunSnapshot | null>(null);
  const snapshotTimerRef = useRef<number | null>(null);

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

      const prevUnlisten = unlistenRef.current;
      unlistenRef.current = () => {
        prevUnlisten?.();
        unlistenSessions();
        unlistenOpenSession();
      };
    };

    setupListener();

    // 加载初始工作空间和当前对话目录
    Promise.all([api.getWorkspaceDir(), api.getSessionCwd()])
      .then(([workspaceDir, sessionCwd]) => {
        useStore.setState({ workspaceDir, sessionCwd });
      })
      .catch(console.error);

    return () => {
      if (snapshotTimerRef.current !== null) {
        window.clearTimeout(snapshotTimerRef.current);
        snapshotTimerRef.current = null;
      }
      unlistenRef.current?.();
    };
  }, []);

  const handleDragStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    draggingRef.current = true;
    startXRef.current = e.clientX;
    startWidthRef.current = browserWidth;

    const onMouseMove = (ev: MouseEvent) => {
      if (!draggingRef.current) return;
      const delta = startXRef.current - ev.clientX;
      const windowWidth = window.innerWidth;
      const maxWidth = windowWidth * BROWSER_MAX_WIDTH_RATIO;
      const next = Math.min(maxWidth, Math.max(BROWSER_MIN_WIDTH, startWidthRef.current + delta));
      setBrowserWidth(next);
    };

    const onMouseUp = () => {
      draggingRef.current = false;
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };

    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  }, [browserWidth]);

  return (
    <SidebarProvider>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        {/* 顶部 Header — 横跨全宽，固定在最顶部 */}
        <LazyStatusPanel
          showBrowser={showBrowser}
          onToggleBrowser={() => setShowBrowser((prev) => !prev)}
        />

        {/* 下方区域：Sidebar + 主内容 */}
        <div className="flex flex-1 min-h-0">
          <AppSidebar />

          {/* 主内容区 */}
          <main className="flex flex-1 flex-col min-w-0 bg-background">
            <div className="flex flex-1 min-h-0">
              <div
                className="flex flex-1 flex-col min-w-0"
                style={showBrowser ? { maxWidth: `calc(100% - ${browserWidth}px - 4px)` } : undefined}
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
                <>
                  {/* 拖拽手柄 */}
                  <div
                    onMouseDown={handleDragStart}
                    className="w-1 shrink-0 cursor-col-resize hover:bg-primary/30 active:bg-primary/50 transition-colors"
                  />
                  <div style={{ width: browserWidth }} className="shrink-0">
                    <BrowserPanel onClose={() => setShowBrowser(false)} />
                  </div>
                </>
              )}
            </div>
          </main>
        </div>
      </div>
    </SidebarProvider>
  );
}
