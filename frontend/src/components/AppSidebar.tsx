import { useStore } from '@/store/useStore';
import { Button } from './ui/button';
import { ScrollArea } from './ui/scroll-area';
import { useSidebar } from './ui/sidebar';
import { Plus, MessageSquare, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { SettingsDialog } from './SettingsDialog';

export function AppSidebar() {
  const {
    sessions,
    activeSessionId,
    isDraft,
    sessionRunStatuses,
    createSession,
    switchSession,
    deleteSession,
    isLoadingSessions,
  } = useStore();

  const { open } = useSidebar();
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  const handleDeleteSession = async () => {
    await deleteSession();
    setShowDeleteConfirm(false);
  };

  return (
    <aside
      className="shrink-0 border-r bg-sidebar text-sidebar-foreground flex flex-col overflow-hidden transition-[width] duration-200 ease-linear"
      style={{ width: open ? 'var(--sidebar-width, 16rem)' : '0px' }}
    >
      <div className="flex flex-col h-full min-w-[var(--sidebar-width,16rem)]">
        {/* 新建会话 */}
        <div className="p-2 shrink-0">
          <Button
            variant="ghost"
            className="w-full justify-start"
            onClick={() => createSession()}
          >
            <Plus className="w-4 h-4 mr-2" />
            新对话
          </Button>
        </div>

        {/* 会话列表 */}
        <ScrollArea className="flex-1 px-2">
          <div className="space-y-1 py-1">
            {isLoadingSessions ? (
              <div className="px-3 py-2 text-sm text-muted-foreground">加载中...</div>
            ) : sessions.length === 0 ? (
              <div className="px-3 py-2 text-sm text-muted-foreground">暂无对话</div>
            ) : (
              [...sessions]
                .filter((s) => s.message_count > 0 || s.id === activeSessionId)
                .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
                .map((session) => {
                const isRunning = !!sessionRunStatuses[session.id];
                return (
                <button
                  key={session.id}
                  className={`w-full text-left px-3 py-2 rounded-md text-sm flex items-center gap-2 group transition-colors ${
                    !isDraft && activeSessionId === session.id
                      ? 'bg-sidebar-accent text-sidebar-accent-foreground'
                      : 'text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
                  }`}
                  onClick={() => {
                    if (session.id !== activeSessionId || isDraft) {
                      switchSession(session.id);
                    }
                  }}
                >
                  <MessageSquare className="w-4 h-4 shrink-0" />
                  <span className="flex-1 truncate">{session.title || '新对话'}</span>
                  {isRunning && (
                    <span className="w-2 h-2 rounded-full bg-yellow-500 animate-pulse shrink-0" />
                  )}
                  {!isDraft && activeSessionId === session.id && (
                    <button
                      className="opacity-0 group-hover:opacity-100 hover:text-destructive transition-opacity"
                      onClick={(e) => {
                        e.stopPropagation();
                        setShowDeleteConfirm(true);
                      }}
                    >
                      <Trash2 className="w-3 h-3" />
                    </button>
                  )}
                </button>
                );
              })
            )}
          </div>
        </ScrollArea>

        {/* 底部设置 */}
        <div className="p-2 border-t border-sidebar-border shrink-0">
          <SettingsDialog />
        </div>
      </div>

      {/* 删除确认 */}
      {showDeleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-popover border rounded-lg p-4 max-w-sm w-full mx-4">
            <h3 className="font-medium mb-2">删除对话</h3>
            <p className="text-muted-foreground text-sm mb-4">确定要删除当前对话吗？此操作无法撤销。</p>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setShowDeleteConfirm(false)}>
                取消
              </Button>
              <Button variant="destructive" onClick={handleDeleteSession}>
                删除
              </Button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}
