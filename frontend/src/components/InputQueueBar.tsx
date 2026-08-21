import { CornerUpLeft, ListOrdered, Paperclip, X } from 'lucide-react';
import { useStore } from '@/store/useStore';

const EMPTY_QUEUE: never[] = [];

interface InputQueueBarProps {
  cacheKey: string | null;
  isIdle: boolean;
}

/** 输入框上方的待投递队列：逐条展示、可单独引导/发送或移除。 */
export function InputQueueBar({ cacheKey, isIdle }: InputQueueBarProps) {
  const queue = useStore((state) => (cacheKey ? state.inputQueues[cacheKey] : undefined)) ?? EMPTY_QUEUE;
  const removeQueuedInputMessage = useStore((state) => state.removeQueuedInputMessage);
  const steerQueuedInputMessage = useStore((state) => state.steerQueuedInputMessage);
  if (!cacheKey || queue.length === 0) return null;
  return (
    <div className="mb-2 flex flex-col gap-1">
      {queue.map((message, index) => (
        <div
          key={message.id}
          className="flex items-center gap-2 rounded-md border bg-muted/30 px-2 py-1.5 text-xs"
        >
          <span className="flex shrink-0 items-center gap-1 text-muted-foreground/70 tabular-nums">
            <ListOrdered className="h-3 w-3" />
            {index + 1}
          </span>
          <span className="min-w-0 flex-1 truncate text-muted-foreground" title={message.text}>
            {message.text.trim() || '（仅附件）'}
          </span>
          {message.attachments.length > 0 && (
            <span
              className="flex shrink-0 items-center gap-0.5 text-muted-foreground/70"
              title={message.attachments.map((item) => item.original_name ?? item.source).join('\n')}
            >
              <Paperclip className="h-3 w-3" />
              {message.attachments.length}
            </span>
          )}
          <button
            type="button"
            onClick={() => { void steerQueuedInputMessage(cacheKey, message.id); }}
            className="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
            title={isIdle ? '立即发送' : '立即引导（打断当前执行）'}
          >
            <CornerUpLeft className="h-3.5 w-3.5" />
          </button>
          <button
            type="button"
            onClick={() => removeQueuedInputMessage(cacheKey, message.id)}
            className="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
            title="从队列移除"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      ))}
    </div>
  );
}
