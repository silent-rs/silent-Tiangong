import { useEffect, useRef, useState, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { api, type RunSnapshot } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { BrowserPanel } from '@/components/BrowserPanel';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import type { UnlistenFn } from '@tauri-apps/api/event';

const CHAT_MIN_WIDTH = 400;
const CHAT_MAX_WIDTH = 800;

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const [showBrowser, setShowBrowser] = useState(false);
  const [browserUrl, setBrowserUrl] = useState<string | undefined>(undefined);
  const [chatWidth, setChatWidth] = useState(CHAT_MAX_WIDTH);
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

      // 监听浏览器自动打开事件
      const unlistenBrowserOpen = await listen<string>('browser:open', (event) => {
        const url = event.payload;
        setBrowserUrl(url);
        setShowBrowser(true);
      });

      const prevUnlisten = unlistenRef.current;
      unlistenRef.current = () => {
        prevUnlisten?.();
        unlistenSessions();
        unlistenOpenSession();
        unlistenBrowserOpen();
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
    const onOpenBrowser = (e: Event) => {
      const url = (e as CustomEvent).detail;
      if (typeof url === 'string') {
        setBrowserUrl(url);
        setShowBrowser(true);
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

  const handleDragStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    draggingRef.current = true;
    startXRef.current = e.clientX;
    startWidthRef.current = chatWidth;

    const onMouseMove = (ev: MouseEvent) => {
      if (!draggingRef.current) return;
      const delta = ev.clientX - startXRef.current;
      const next = Math.min(CHAT_MAX_WIDTH, Math.max(CHAT_MIN_WIDTH, startWidthRef.current + delta));
      setChatWidth(next);
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
  }, [chatWidth]);

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
                className={`flex flex-col min-w-0 ${showBrowser ? 'shrink-0' : 'flex-1'}`}
                style={showBrowser ? { width: chatWidth } : undefined}
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
                  <div className="flex-1 min-w-0">
                    <BrowserPanel onClose={() => { setShowBrowser(false); setBrowserUrl(undefined); }} initialUrl={browserUrl} />
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
