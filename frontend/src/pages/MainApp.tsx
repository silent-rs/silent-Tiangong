import { useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { api, type RunSnapshot } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import type { UnlistenFn } from '@tauri-apps/api/event';

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
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

    // 定期刷新会话列表，确保自动化任务创建的新会话能及时显示
    const refreshTimer = window.setInterval(() => {
      loadSessions();
    }, 30_000);

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
      window.clearInterval(refreshTimer);
      unlistenRef.current?.();
    };
  }, []);

  return (
    <SidebarProvider>
      <div className="flex flex-col h-screen w-full overflow-hidden">
        {/* 顶部 Header — 横跨全宽，固定在最顶部 */}
        <LazyStatusPanel />

        {/* 下方区域：Sidebar + 主内容 */}
        <div className="flex flex-1 min-h-0">
          <AppSidebar />

          {/* 主内容区 */}
          <main className="flex flex-1 flex-col min-w-0 bg-background">
            {/* 消息列表 */}
            <div className="flex-1 overflow-hidden">
              <LazyMessageList />
            </div>

            {/* 输入框 */}
            <LazyMessageInput />
          </main>
        </div>
      </div>
    </SidebarProvider>
  );
}
