import { useState, KeyboardEvent, useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square } from 'lucide-react';

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus, isDraft, activeSessionId, sessionRunStatuses } = useStore();
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
    if (e.key === 'Enter' && !e.shiftKey && !isComposing) {
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
                ? '输入消息... (Enter 发送, Shift+Enter 换行)'
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
          <span>
            {!isIdle && (
              <span className="flex items-center gap-1">
                <span className="w-2 h-2 rounded-full bg-yellow-500 animate-pulse" />
                {currentSessionStatus === 'planning' && '正在制定计划...'}
                {currentSessionStatus === 'executing' && '正在执行任务...'}
                {currentSessionStatus === 'responding' && '正在生成回复...'}
              </span>
            )}
          </span>
          <span>Shift+Enter 换行</span>
        </div>
      </div>
    </div>
  );
}
