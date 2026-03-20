import { useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { Sidebar } from '@/components/Sidebar';
import { LazyMessageList, LazyMessageInput, LazyPlanPanel, LazyStatusPanel } from '@/components/LazyComponents';
import type { UnlistenFn } from '@tauri-apps/api/event';

export function MainApp() {
  const { loadSessions, updateFromSnapshot } = useStore();
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    // 加载会话列表
    loadSessions();

    // 监听运行状态更新
    const setupListener = async () => {
      const unlisten = await api.onRunSnapshot((snapshot) => {
        updateFromSnapshot(snapshot);
      });
      unlistenRef.current = unlisten;
    };

    setupListener();

    // 初始获取状态
    api.getRunSnapshot().then((snapshot) => {
      updateFromSnapshot(snapshot);
    }).catch(console.error);

    return () => {
      unlistenRef.current?.();
    };
  }, []);

  return (
    <div className="flex h-screen bg-[#1E1E1E] text-white overflow-hidden">
      {/* 侧边栏 */}
      <Sidebar />

      {/* 主内容区 */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* 顶部状态栏 */}
        <LazyStatusPanel />

        {/* 计划面板（执行时显示） */}
        <LazyPlanPanel />

        {/* 消息列表 */}
        <LazyMessageList />

        {/* 输入框 */}
        <LazyMessageInput />
      </div>
    </div>
  );
}
