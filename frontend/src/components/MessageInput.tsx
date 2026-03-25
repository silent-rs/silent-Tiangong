import { useState, KeyboardEvent, useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square, FolderOpen } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus, isDraft, activeSessionId, sessionRunStatuses, sessionCwd, setSessionCwd } = useStore();
  const [isComposing, setIsComposing] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // 当前会话是否空闲：草稿模式一定空闲，否则检查 per-session 状态
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

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !isComposing) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleSend = () => {
    if (canSend) {
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

  // 显示简短路径（只取最后两段）
  const displayCwd = sessionCwd
    ? sessionCwd.split('/').filter(Boolean).slice(-2).join('/')
    : '';

  return (
    <div className="p-4 border-t bg-background">
      <div className="max-w-3xl mx-auto">
        <div className="relative">
          <Textarea
            ref={textareaRef}
            value={inputContent}
            onChange={(e) => setInputContent(e.target.value)}
            onKeyDown={handleKeyDown}
            onCompositionStart={() => setIsComposing(true)}
            onCompositionEnd={() => setIsComposing(false)}
            placeholder={
              isIdle
                ? '输入消息... (⌘+Enter 发送)'
                : '正在执行中...'
            }
            className="min-h-[60px] max-h-[200px] resize-none pr-14 bg-muted/50 focus-visible:ring-ring"
            disabled={!isIdle}
          />
          {/* 发送/取消按钮 - 嵌在输入框内右下角 */}
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
