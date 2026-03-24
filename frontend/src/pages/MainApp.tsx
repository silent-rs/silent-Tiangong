import { useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { AppSidebar } from '@/components/AppSidebar';
import { SidebarProvider } from '@/components/ui/sidebar';
import { LazyMessageList, LazyMessageInput, LazyPlanPanel, LazyStatusPanel } from '@/components/LazyComponents';
import type { UnlistenFn } from '@tauri-apps/api/event';

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    loadSessions();

    const setupListener = async () => {
      const unlisten = await api.onRunSnapshot((snapshot) => {
        updateFromSnapshot(snapshot);
      });
      unlistenRef.current = unlisten;
    };

    setupListener();

    api.getRunSnapshot().then((snapshot) => {
      updateFromSnapshot(snapshot);
    }).catch(console.error);

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
            {/* 计划面板（执行时显示） */}
            <LazyPlanPanel />

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
