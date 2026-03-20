import { useState, useRef, useEffect, KeyboardEvent } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Circle } from 'lucide-react';

const appWindow = getCurrentWindow();

export function StatusPanel() {
  const { runStatus, activeSessionId, sessions, loadSessions } = useStore();
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const savingRef = useRef(false);

  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const currentTitle = activeSession?.title || '新对话';

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  useEffect(() => {
    setIsEditing(false);
  }, [activeSessionId]);

  const startEditing = () => {
    if (!activeSession) return;
    setEditValue(currentTitle);
    setIsEditing(true);
  };

  const saveTitle = async () => {
    if (savingRef.current) return;
    savingRef.current = true;

    setIsEditing(false);
    const trimmed = editValue.trim();
    const finalTitle = trimmed || '新对话';

    if (finalTitle !== currentTitle) {
      try {
        await api.updateSessionTitle(finalTitle);
        await loadSessions();
      } catch (e) {
        console.error('保存标题失败:', e);
      }
    }

    savingRef.current = false;
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      inputRef.current?.blur();
    } else if (e.key === 'Escape') {
      setIsEditing(false);
      savingRef.current = false;
    }
  };

  const getStatusColor = () => {
    switch (runStatus) {
      case 'planning':
      case 'executing':
        return 'text-[#FFC107]';
      case 'failed':
        return 'text-[#F44336]';
      case 'completed':
        return 'text-[#4CAF50]';
      default:
        return 'text-[#4CAF50]';
    }
  };

  const getStatusText = () => {
    switch (runStatus) {
      case 'planning':
        return '计划中';
      case 'executing':
        return '执行中';
      case 'completed':
        return '已完成';
      case 'failed':
        return '执行失败';
      default:
        return '就绪';
    }
  };

  return (
    <div
      className="h-10 border-b border-[#3C3C3C] bg-[#1E1E1E] flex items-center px-4 justify-between select-none"
      onMouseDown={(e) => {
        // 排除交互元素
        const tag = (e.target as HTMLElement).tagName;
        if (tag === 'INPUT' || tag === 'BUTTON') return;
        if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
        appWindow.startDragging();
      }}
    >
      <div className="flex items-center gap-3">
        {activeSession && (
          isEditing ? (
            <input
              ref={inputRef}
              value={editValue}
              onChange={(e) => setEditValue(e.target.value)}
              onBlur={saveTitle}
              onKeyDown={handleKeyDown}
              className="text-sm text-[#CCCCCC] bg-transparent border-b border-[#10A37F] outline-none max-w-[300px] py-0.5"
            />
          ) : (
            <span
              data-no-drag
              className="text-sm text-[#CCCCCC] truncate max-w-[300px] cursor-pointer hover:text-white"
              onClick={startEditing}
              title="点击编辑标题"
            >
              {currentTitle}
            </span>
          )
        )}
      </div>
      <div className="flex items-center gap-2">
        <Circle className={`w-2 h-2 ${getStatusColor()} ${runStatus !== 'idle' ? 'animate-pulse' : 'fill-current'}`} />
        <span className="text-sm text-[#858585]">{getStatusText()}</span>
      </div>
    </div>
  );
}
