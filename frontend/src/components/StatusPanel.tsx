import { useState, useRef, useEffect, KeyboardEvent } from 'react';
import { useStore } from '@/store/useStore';
import { api } from '@/api/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Sun, Moon, Monitor, PanelLeft, SquarePen, Volume2, VolumeX, AudioLines, Globe, ArrowUpCircle, Search, TerminalSquare } from 'lucide-react';
import { useTheme } from '@/hooks/useTheme';
import { useSearchStore } from '@/store/useSearchStore';
import { useStreamingTts } from '@/hooks/useStreamingTts';
import { useToast } from './Toast';
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

interface StatusPanelProps {
  browserActive?: boolean;
  onOpenBrowser?: () => void;
  terminalActive?: boolean;
  onOpenTerminal?: () => void;
}

function SearchButton() {
  const searchActive = useSearchStore((s) => s.searchActive);
  return (
    <button
      data-no-drag
      onClick={() => {
        const store = useSearchStore.getState();
        store.searchActive ? store.closeSearch() : store.openSearch();
      }}
      className={`transition-colors ${
        searchActive
          ? 'text-primary'
          : 'text-muted-foreground hover:text-foreground'
      }`}
      title="搜索消息"
    >
      <Search className="w-4 h-4" />
    </button>
  );
}

export function StatusPanel({ browserActive, onOpenBrowser, terminalActive, onOpenTerminal }: StatusPanelProps) {
  const { activeSessionId, isDraft, sessions, loadSessions, createSession, updateAvailable, setPendingSettingsTab } = useStore();
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const savingRef = useRef(false);

  const { theme, setTheme } = useTheme();
  const { toggleSidebar, open: sidebarOpen } = useSidebar();
  const streamingTts = useStreamingTts();
  const [hasTts, setHasTts] = useState(false);
  // 终端绿点：有任意会话 PTY 处于 Interactive/Running 时点亮
  const [terminalBusy, setTerminalBusy] = useState(false);
  const toast = useToast();
  const lastBusyRef = useRef(false);

  useEffect(() => {
    api.hasTtsCapability().then(setHasTts).catch(() => setHasTts(false));
  }, []);

  // 轮询终端状态（每 1.5s）→ 绿点 + toast 提示后台命令
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const statuses = await api.terminalListStatuses();
        if (cancelled) return;
        const busy = statuses.some(
          (s) => s.alive && s.phase === 'Running',
        );
        setTerminalBusy(busy);
        // 从空闲变忙时弹一次 toast（且当前面板未开）
        if (busy && !lastBusyRef.current && !terminalActive) {
          toast.showInfo('命令在终端执行中', '点击终端按钮查看输出', 2500);
        }
        lastBusyRef.current = busy;
      } catch {
        // plugin 未注册时静默
      }
    };
    poll();
    const id = window.setInterval(poll, 1500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [terminalActive, toast]);

  const activeSession = isDraft ? null : sessions.find((s) => s.id === activeSessionId);
  const currentTitle = isDraft ? '新对话' : (activeSession?.title || '新对话');



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


  const titlePaddingLeft = navigator.platform.includes('Mac') ? '80px' : '16px';

  return (
    <header
      className="flex h-12 shrink-0 items-center gap-2 border-b pr-4 select-none"
      style={{ paddingLeft: titlePaddingLeft }}
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
        {updateAvailable && (
          <button
            data-no-drag
            onClick={() => setPendingSettingsTab('about')}
            className="flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium text-white bg-sky-500 hover:bg-sky-400 transition-colors"
            title={`发现新版本 ${updateAvailable.version}，点击查看`}
          >
            <ArrowUpCircle className="w-3.5 h-3.5" />
            <span>v{updateAvailable.version}</span>
          </button>
        )}
        <SearchButton />
        {onOpenTerminal && (
          <button
            data-no-drag
            onClick={onOpenTerminal}
            className={`relative transition-colors ${
              terminalActive
                ? 'text-primary'
                : 'text-muted-foreground hover:text-foreground'
            }`}
            title={terminalActive ? '终端已打开' : '打开终端'}
          >
            <TerminalSquare className="w-4 h-4" />
            {terminalBusy && !terminalActive && (
              <span
                className="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-emerald-500 animate-pulse"
                title="有命令在终端执行中"
              />
            )}
          </button>
        )}
        <button
          data-no-drag
          onClick={onOpenBrowser}
          className={`transition-colors ${
            browserActive
              ? 'text-primary'
              : 'text-muted-foreground hover:text-foreground'
          }`}
          title={browserActive ? '浏览器已打开' : '打开浏览器'}
        >
          <Globe className="w-4 h-4" />
        </button>
        <button
          data-no-drag
          onClick={cycleTheme}
          className="text-muted-foreground hover:text-foreground transition-colors"
          title={themeLabel}
        >
          <ThemeIcon className="w-4 h-4" />
        </button>
      </div>
    </header>
  );
}
