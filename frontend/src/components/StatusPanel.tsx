import { useState, useRef, useEffect, KeyboardEvent } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Circle, Sun, Moon, Monitor, PanelLeft, SquarePen, Volume2, VolumeX, AudioLines } from 'lucide-react';
import { useTheme } from '@/hooks/useTheme';
import { useStreamingTts } from '@/hooks/useStreamingTts';
import { Separator } from './ui/separator';
import { useSidebar } from './ui/sidebar';
import { Button } from './ui/button';
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
} from './ui/breadcrumb';

const appWindow = getCurrentWindow();

export function StatusPanel() {
  const { runStatus, runSummary, activeSessionId, isDraft, sessionRunStatuses, sessions, loadSessions, createSession } = useStore();
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const savingRef = useRef(false);

  const { theme, setTheme } = useTheme();
  const { toggleSidebar, open: sidebarOpen } = useSidebar();
  const streamingTts = useStreamingTts();
  const [hasTts, setHasTts] = useState(false);

  useEffect(() => {
    api.hasTtsCapability().then(setHasTts).catch(() => setHasTts(false));
  }, []);
  const activeSession = isDraft ? null : sessions.find((s) => s.id === activeSessionId);
  const currentTitle = isDraft ? '新对话' : (activeSession?.title || '新对话');

  // 当前会话的运行状态
  const currentRunStatus = isDraft
    ? 'idle'
    : (activeSessionId && sessionRunStatuses[activeSessionId]) || runStatus;

  const cycleTheme = () => {
    const next = theme === 'dark' ? 'light' : theme === 'light' ? 'system' : 'dark';
    setTheme(next);
  };
  const ThemeIcon = theme === 'dark' ? Moon : theme === 'light' ? Sun : Monitor;
  const themeLabel = theme === 'dark' ? '深色模式' : theme === 'light' ? '浅色模式' : '跟随系统';

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
    switch (currentRunStatus) {
      case 'planning':
      case 'executing':
        return 'text-yellow-500';
      case 'failed':
        return 'text-destructive';
      case 'completed':
      default:
        return 'text-green-500';
    }
  };

  const getStatusText = () => {
    switch (currentRunStatus) {
      case 'planning':
      case 'executing':
        // 显示具体执行内容（如"正在执行：pipx run ..."）
        if (runSummary && runSummary !== '正在处理') {
          const truncated = runSummary.length > 40 ? runSummary.slice(0, 40) + '...' : runSummary;
          return truncated;
        }
        return '处理中';
      case 'completed':
        return '已完成';
      case 'failed':
        return '失败';
      default:
        return '就绪';
    }
  };

  return (
    <header
      className="flex h-12 shrink-0 items-center gap-2 border-b pr-4 select-none"
      style={{ paddingLeft: '80px' }}
      onMouseDown={(e) => {
        const tag = (e.target as HTMLElement).tagName;
        if (tag === 'INPUT' || tag === 'BUTTON') return;
        if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
        appWindow.startDragging();
      }}
    >
      <div className="flex items-center gap-2">
        <Button
          data-no-drag
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          onClick={toggleSidebar}
        >
          <PanelLeft className="h-4 w-4" />
          <span className="sr-only">切换侧边栏</span>
        </Button>
        {!sidebarOpen && (
          <Button
            data-no-drag
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={() => createSession()}
            title="新对话"
          >
            <SquarePen className="h-4 w-4" />
          </Button>
        )}
        <Separator orientation="vertical" className="mr-2 h-4" />
        <Breadcrumb>
          <BreadcrumbList>
            <BreadcrumbItem>
              {(isDraft || activeSession) && (
                isDraft ? (
                  <BreadcrumbPage>新对话</BreadcrumbPage>
                ) : isEditing ? (
                  <input
                    ref={inputRef}
                    value={editValue}
                    onChange={(e) => setEditValue(e.target.value)}
                    onBlur={saveTitle}
                    onKeyDown={handleKeyDown}
                    className="text-sm text-foreground bg-transparent border-b border-primary outline-none max-w-[300px] py-0.5"
                  />
                ) : (
                  <BreadcrumbPage
                    data-no-drag
                    className="cursor-pointer hover:text-foreground"
                    onClick={startEditing}
                    title="点击编辑标题"
                  >
                    {currentTitle}
                  </BreadcrumbPage>
                )
              )}
            </BreadcrumbItem>
          </BreadcrumbList>
        </Breadcrumb>
      </div>

      <div className="ml-auto flex items-center gap-3">
        {hasTts && (
          <button
            data-no-drag
            onClick={() => streamingTts.enabled ? streamingTts.stop() : streamingTts.setEnabled(true)}
            className={`flex items-center gap-1 px-2 py-0.5 rounded text-xs transition-colors ${
              streamingTts.enabled
                ? 'bg-primary text-primary-foreground'
                : 'text-muted-foreground hover:text-foreground'
            }`}
            title={streamingTts.enabled ? '关闭自动朗读' : '开启自动朗读'}
          >
            {streamingTts.enabled ? (
              streamingTts.isPlaying
                ? <AudioLines className="w-3.5 h-3.5 animate-pulse" />
                : <Volume2 className="w-3.5 h-3.5" />
            ) : (
              <VolumeX className="w-3.5 h-3.5" />
            )}
          </button>
        )}
        <button
          data-no-drag
          onClick={cycleTheme}
          className="text-muted-foreground hover:text-foreground transition-colors"
          title={themeLabel}
        >
          <ThemeIcon className="w-4 h-4" />
        </button>
        <div className="flex items-center gap-2">
          <Circle className={`w-2 h-2 ${getStatusColor()} ${currentRunStatus !== 'idle' ? 'animate-pulse' : 'fill-current'}`} />
          <span className="text-sm text-muted-foreground">{getStatusText()}</span>
        </div>
      </div>
    </header>
  );
}
