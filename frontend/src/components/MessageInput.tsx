import { useState, KeyboardEvent, useEffect, useRef } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square } from 'lucide-react';

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus } = useStore();
  const [isComposing, setIsComposing] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const isIdle = runStatus === 'idle';

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
    if (inputContent.trim() && isIdle) {
      sendMessage(inputContent);
    }
  };

  const handleCancel = () => {
    cancelTurn();
  };

  return (
    <div className="p-4 border-t border-[#3C3C3C] bg-[#1E1E1E]">
      <div className="max-w-3xl mx-auto">
        <div className="flex gap-2 items-end">
          <div className="flex-1 relative">
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
              className="min-h-[60px] max-h-[200px] bg-[#2D2D30] border-[#3C3C3C] text-white placeholder:text-[#858585] resize-none pr-12"
              disabled={!isIdle}
            />
            {/* 字符计数 */}
            {inputContent.length > 0 && (
              <div className="absolute bottom-2 right-2 text-xs text-[#858585]">
                {inputContent.length}
              </div>
            )}
          </div>
          <Button
            onClick={isIdle ? handleSend : handleCancel}
            disabled={isIdle && !inputContent.trim()}
            className={`self-end h-[60px] w-[60px] ${
              isIdle
                ? 'bg-[#10A37F] hover:bg-[#0D8A6A]'
                : 'bg-[#F44336] hover:bg-[#D32F2F]'
            }`}
          >
            {isIdle ? (
              <Send className="w-5 h-5" />
            ) : (
              <Square className="w-5 h-5" />
            )}
          </Button>
        </div>
        <div className="mt-2 flex items-center justify-between text-xs text-[#858585]">
          <span>
            {!isIdle && (
              <span className="flex items-center gap-1">
                <span className="w-2 h-2 rounded-full bg-[#FFC107] animate-pulse" />
                {runStatus === 'planning' && '正在制定计划...'}
                {runStatus === 'executing' && '正在执行任务...'}
                {runStatus === 'responding' && '正在生成回复...'}
              </span>
            )}
          </span>
          <span>Shift+Enter 换行</span>
        </div>
      </div>
    </div>
  );
}
