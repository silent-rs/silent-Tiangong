import { useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyStatusPanel } from '@/components/LazyComponents';
import { ensureDesktopNotificationPermission } from '@/utils/desktopNotification';
import type { UnlistenFn } from '@tauri-apps/api/event';

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    ensureDesktopNotificationPermission().catch(console.warn);

    loadSessions();

    const setupListener = async () => {
      const unlisten = await api.onRunSnapshot((snapshot) => {
        updateFromSnapshot(snapshot);
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

    api.getRunSnapshot().then((snapshot) => {
      updateFromSnapshot(snapshot);
    }).catch(console.error);

    // 加载初始工作空间和当前对话目录
    Promise.all([api.getWorkspaceDir(), api.getSessionCwd()])
      .then(([workspaceDir, sessionCwd]) => {
        useStore.setState({ workspaceDir, sessionCwd });
      })
      .catch(console.error);

    return () => {
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
