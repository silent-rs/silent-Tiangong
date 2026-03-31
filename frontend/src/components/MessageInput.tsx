import { useState, KeyboardEvent, useEffect, useRef, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square, FolderOpen, Wrench, Cpu } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { api } from '@/api/tauri';

interface MentionCandidate {
  value: string;
  label: string;
  kind: string;
  hint: string;
}

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus, isDraft, activeSessionId, sessionRunStatuses, sessionCwd, setSessionCwd } = useStore();
  const [isComposing, setIsComposing] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // @提及补全状态
  const [mentionOpen, setMentionOpen] = useState(false);
  const [mentionCandidates, setMentionCandidates] = useState<MentionCandidate[]>([]);
  const [mentionFilter, setMentionFilter] = useState('');
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionStart, setMentionStart] = useState(-1); // @ 符号在 inputContent 中的位置
  const mentionRef = useRef<HTMLDivElement>(null);

  // 当前会话是否空闲
  const currentSessionStatus = isDraft
    ? 'idle'
    : (activeSessionId && sessionRunStatuses[activeSessionId]) || runStatus;
  const isIdle = currentSessionStatus === 'idle';
  const canSend = isIdle && inputContent.trim().length > 0;

  // 自动调整文本框高度
  useEffect(() => {
    const textarea = textareaRef.current;
    if (textarea) {
      textarea.style.height = '60px';
      textarea.style.height = Math.min(textarea.scrollHeight, 200) + 'px';
    }
  }, [inputContent]);

  // 加载候选列表
  const loadCandidates = useCallback(async () => {
    try {
      const candidates = await api.getMentionCandidates();
      setMentionCandidates(candidates);
    } catch (e) {
      console.error('加载提及候选失败:', e);
    }
  }, []);

  // 过滤后的候选列表
  const filteredCandidates = mentionCandidates.filter(c => {
    if (!mentionFilter) return true;
    const lower = mentionFilter.toLowerCase();
    return c.label.toLowerCase().includes(lower)
      || c.value.toLowerCase().includes(lower)
      || c.hint.toLowerCase().includes(lower);
  });

  // 检测 @ 输入
  const handleInputChange = (value: string) => {
    setInputContent(value);

    const textarea = textareaRef.current;
    if (!textarea) return;
    const cursorPos = textarea.selectionStart;

    // 向前搜索 @ 符号
    let atPos = -1;
    for (let i = cursorPos - 1; i >= 0; i--) {
      const ch = value[i];
      if (ch === '@') {
        // @ 前面是空格或行首
        if (i === 0 || /\s/.test(value[i - 1])) {
          atPos = i;
        }
        break;
      }
      if (/\s/.test(ch)) break;
    }

    if (atPos >= 0) {
      const filter = value.slice(atPos + 1, cursorPos);
      setMentionStart(atPos);
      setMentionFilter(filter);
      setMentionIndex(0);
      if (!mentionOpen) {
        loadCandidates();
        setMentionOpen(true);
      }
    } else {
      setMentionOpen(false);
    }
  };

  // 选择候选项
  const selectCandidate = (candidate: MentionCandidate) => {
    if (mentionStart < 0) return;
    const textarea = textareaRef.current;
    const cursorPos = textarea?.selectionStart ?? inputContent.length;
    const before = inputContent.slice(0, mentionStart);
    const after = inputContent.slice(cursorPos);
    const newValue = `${before}${candidate.value} ${after}`;
    setInputContent(newValue);
    setMentionOpen(false);

    // 聚焦并设置光标位置
    setTimeout(() => {
      if (textarea) {
        const newPos = mentionStart + candidate.value.length + 1;
        textarea.focus();
        textarea.setSelectionRange(newPos, newPos);
      }
    }, 0);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // 提及补全导航
    if (mentionOpen && filteredCandidates.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setMentionIndex(i => (i + 1) % filteredCandidates.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setMentionIndex(i => (i - 1 + filteredCandidates.length) % filteredCandidates.length);
        return;
      }
      if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey) {
        e.preventDefault();
        selectCandidate(filteredCandidates[mentionIndex]);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setMentionOpen(false);
        return;
      }
      if (e.key === 'Tab') {
        e.preventDefault();
        selectCandidate(filteredCandidates[mentionIndex]);
        return;
      }
    }

    // 发送消息
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !isComposing) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleSend = () => {
    if (canSend) {
      setMentionOpen(false);
      sendMessage(inputContent);
    }
  };

  const handleCancel = () => {
    cancelTurn();
  };

  const handleChangeCwd = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: sessionCwd || undefined,
        title: '选择工作目录',
      });
      if (selected && typeof selected === 'string') {
        await setSessionCwd(selected);
      }
    } catch (error) {
      console.error('选择目录失败:', error);
    }
  };

  const displayCwd = sessionCwd
    ? sessionCwd.split('/').filter(Boolean).slice(-2).join('/')
    : '';

  return (
    <div className="p-4 border-t bg-background">
      <div className="max-w-3xl mx-auto">
        <div className="relative">
          {/* @提及补全下拉列表 */}
          {mentionOpen && filteredCandidates.length > 0 && (
            <div
              ref={mentionRef}
              className="absolute bottom-full left-0 mb-1 w-72 max-h-48 overflow-y-auto rounded-md border bg-popover shadow-lg z-50"
            >
              {filteredCandidates.map((c, i) => (
                <button
                  key={c.value}
                  className={`w-full flex items-center gap-2 px-3 py-1.5 text-sm text-left hover:bg-accent transition-colors ${
                    i === mentionIndex ? 'bg-accent' : ''
                  }`}
                  onMouseDown={(e) => {
                    e.preventDefault(); // 阻止 blur
                    selectCandidate(c);
                  }}
                  onMouseEnter={() => setMentionIndex(i)}
                >
                  {c.kind === 'skill' ? (
                    <Wrench className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                  ) : (
                    <Cpu className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                  )}
                  <div className="flex-1 min-w-0">
                    <span className="font-medium">{c.label}</span>
                    <span className="ml-2 text-muted-foreground text-xs truncate">{c.hint}</span>
                  </div>
                </button>
              ))}
            </div>
          )}

          <Textarea
            ref={textareaRef}
            value={inputContent}
            onChange={(e) => handleInputChange(e.target.value)}
            onKeyDown={handleKeyDown}
            onCompositionStart={() => setIsComposing(true)}
            onCompositionEnd={() => setIsComposing(false)}
            onBlur={() => setTimeout(() => setMentionOpen(false), 150)}
            placeholder={
              isIdle
                ? '输入消息... (⌘+Enter 发送，@ 引用 Skill/MCP)'
                : '正在执行中...'
            }
            className="min-h-[60px] max-h-[200px] resize-none pr-14 bg-muted/50 focus-visible:ring-ring"
            disabled={!isIdle}
          />
          {/* 发送/取消按钮 */}
          <Button
            onClick={isIdle ? handleSend : handleCancel}
            disabled={isIdle && !canSend}
            size="icon"
            className={`absolute right-2 bottom-2 h-8 w-8 rounded-md ${
              !isIdle
                ? 'bg-destructive hover:bg-destructive/90 text-destructive-foreground'
                : canSend
                  ? 'bg-green-600 hover:bg-green-700 text-white'
                  : 'bg-muted text-muted-foreground'
            }`}
          >
            {isIdle ? (
              <Send className="w-4 h-4" />
            ) : (
              <Square className="w-4 h-4" />
            )}
          </Button>
        </div>
        <div className="mt-1.5 flex items-center justify-between text-xs text-muted-foreground">
          <button
            onClick={handleChangeCwd}
            disabled={!isIdle}
            className="flex items-center gap-1 hover:text-foreground transition-colors truncate max-w-[300px] disabled:opacity-50 disabled:cursor-default disabled:hover:text-muted-foreground"
            title={sessionCwd || '点击设置工作目录'}
          >
            <FolderOpen className="w-3 h-3 shrink-0" />
            <span className="truncate">{displayCwd || '设置工作目录'}</span>
          </button>
          <span>⌘+Enter 发送</span>
        </div>
      </div>
    </div>
  );
}
