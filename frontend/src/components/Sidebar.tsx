import { useStore } from '@/store/useStore';
import { Button } from './ui/button';
import { ScrollArea } from './ui/scroll-area';
import { Plus, MessageSquare, Trash2 } from 'lucide-react';
import { useState } from 'react';
import { SettingsDialog } from './SettingsDialog';
import { getCurrentWindow } from '@tauri-apps/api/window';

const appWindow = getCurrentWindow();

export function Sidebar() {
  const {
    sessions,
    activeSessionId,
    createSession,
    switchSession,
    deleteSession,
    isLoadingSessions,
  } = useStore();

  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  const handleDeleteSession = async () => {
    await deleteSession();
    setShowDeleteConfirm(false);
  };

  const handleNewSession = async () => {
    await createSession();
  };

  const handleSwitchSession = async (id: string) => {
    if (id !== activeSessionId) {
      await switchSession(id);
    }
  };

  return (
    <div className="w-[240px] bg-[#252526] border-r border-[#3C3C3C] flex flex-col select-none">
      {/* 顶部拖拽区域，为 macOS 红绿灯留空间 */}
      <div
        className="h-10 flex-shrink-0"
        onMouseDown={() => appWindow.startDragging()}
      />

      {/* 新建会话按钮 */}
      <div className="p-2">
        <Button
          variant="ghost"
          className="w-full justify-start text-[#CCCCCC] hover:bg-[#2A2D2E] hover:text-white"
          onClick={handleNewSession}
        >
          <Plus className="w-4 h-4 mr-2" />
          新对话
        </Button>
      </div>

      {/* 会话列表 */}
      <ScrollArea className="flex-1 px-2">
        <div className="space-y-1 py-2">
          {isLoadingSessions ? (
            <div className="px-3 py-2 text-sm text-[#858585]">加载中...</div>
          ) : sessions.length === 0 ? (
            <div className="px-3 py-2 text-sm text-[#858585]">暂无对话</div>
          ) : (
            sessions.map((session) => (
              <button
                key={session.id}
                className={`w-full text-left px-3 py-2 rounded text-sm flex items-center gap-2 group ${
                  activeSessionId === session.id
                    ? 'bg-[#37373D] text-white'
                    : 'text-[#CCCCCC] hover:bg-[#2A2D2E] hover:text-white'
                }`}
                onClick={() => handleSwitchSession(session.id)}
              >
                <MessageSquare className="w-4 h-4 flex-shrink-0" />
                <span className="flex-1 truncate">{session.title || '新对话'}</span>
                {activeSessionId === session.id && (
                  <button
                    className="opacity-0 group-hover:opacity-100 hover:text-red-400 transition-opacity"
                    onClick={(e) => {
                      e.stopPropagation();
                      setShowDeleteConfirm(true);
                    }}
                  >
                    <Trash2 className="w-3 h-3" />
                  </button>
                )}
              </button>
            ))
          )}
        </div>
      </ScrollArea>

      {/* 底部设置按钮 */}
      <div className="p-2 border-t border-[#3C3C3C]">
        <SettingsDialog />
      </div>

      {/* 删除确认对话框 */}
      {showDeleteConfirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-[#252526] border border-[#3C3C3C] rounded-lg p-4 max-w-sm w-full mx-4">
            <h3 className="text-white font-medium mb-2">删除对话</h3>
            <p className="text-[#CCCCCC] text-sm mb-4">确定要删除当前对话吗？此操作无法撤销。</p>
            <div className="flex justify-end gap-2">
              <Button
                variant="ghost"
                className="text-[#CCCCCC] hover:text-white"
                onClick={() => setShowDeleteConfirm(false)}
              >
                取消
              </Button>
              <Button
                variant="ghost"
                className="text-red-400 hover:text-red-300 hover:bg-red-500/10"
                onClick={handleDeleteSession}
              >
                删除
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
