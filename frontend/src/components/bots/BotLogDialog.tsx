import { useCallback, useEffect, useRef, useState } from 'react';
import { AlertTriangle, Loader2, RefreshCw } from 'lucide-react';
import { api, type BotLog } from '../../api/tauri';
import { Button } from '../ui/button';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../ui/dialog';
import { ScrollArea } from '../ui/scroll-area';

interface Props {
  botId: string;
  onClose: () => void;
}

export function BotLogDialog({ botId, onClose }: Props) {
  const [log, setLog] = useState<BotLog>({ content: '', truncated: false });
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const viewportRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setLog(await api.botLog(botId));
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [botId]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      const viewport = viewportRef.current;
      if (viewport) viewport.scrollTop = viewport.scrollHeight;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [log.content]);

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="flex h-[70vh] min-h-[360px] max-h-[640px] max-w-4xl flex-col overflow-hidden">
        <DialogHeader className="mb-0 shrink-0 pr-8">
          <div className="flex items-center justify-between gap-3">
            <DialogTitle className="truncate">{botId} 日志</DialogTitle>
            <div className="flex shrink-0 items-center gap-2">
              {log.truncated && !error && (
                <span className="text-xs text-muted-foreground">仅显示最近内容</span>
              )}
              <Button
                size="icon"
                variant="ghost"
                className="h-8 w-8"
                onClick={() => void load()}
                disabled={loading}
                title="刷新日志"
                aria-label="刷新日志"
              >
                <RefreshCw className={loading ? 'animate-spin' : ''} />
              </Button>
            </div>
          </div>
        </DialogHeader>

        <ScrollArea
          className="mt-4 min-h-0 flex-1 rounded-md border bg-zinc-950"
          viewportRef={viewportRef}
        >
          {loading && !log.content ? (
            <div className="flex h-full min-h-64 items-center justify-center text-zinc-400">
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              正在加载日志
            </div>
          ) : error ? (
            <div className="flex min-h-64 flex-col items-center justify-center gap-3 px-6 text-center text-zinc-300">
              <AlertTriangle className="h-5 w-5 text-red-400" />
              <p className="max-w-xl break-words text-sm">{error}</p>
              <Button size="sm" variant="secondary" onClick={() => void load()}>
                重新读取
              </Button>
            </div>
          ) : log.content ? (
            <pre className="min-w-full whitespace-pre-wrap break-words p-4 font-mono text-xs leading-5 text-zinc-100">
              {log.content}
            </pre>
          ) : (
            <div className="flex min-h-64 items-center justify-center text-sm text-zinc-400">
              暂无日志
            </div>
          )}
        </ScrollArea>
      </DialogContent>
    </Dialog>
  );
}
