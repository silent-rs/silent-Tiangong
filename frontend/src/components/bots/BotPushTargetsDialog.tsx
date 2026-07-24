import { useCallback, useEffect, useState } from 'react';
import { AlertTriangle, Loader2, RefreshCw, Trash2 } from 'lucide-react';
import { api, type BotPushTarget } from '../../api/tauri';
import { Badge } from '../ui/badge';
import { Button } from '../ui/button';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../ui/dialog';
import { ScrollArea } from '../ui/scroll-area';

interface Props {
  botId: string;
  botName: string;
  onClose: () => void;
}

export function BotPushTargetsDialog({ botId, botName, onClose }: Props) {
  const [targets, setTargets] = useState<BotPushTarget[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setTargets(await api.botPushTargets(botId));
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [botId]);

  useEffect(() => {
    void load();
  }, [load]);

  const deleteTarget = async (target: BotPushTarget) => {
    if (!confirm(`确定删除“${target.label}”的推送授权？`)) return;
    setDeleting((current) => new Set(current).add(target.target_id));
    setError(null);
    try {
      await api.botDeletePushTarget(botId, target.target_id);
      setTargets((current) => current.filter((item) => item.target_id !== target.target_id));
    } catch (err) {
      setError(String(err));
    } finally {
      setDeleting((current) => {
        const next = new Set(current);
        next.delete(target.target_id);
        return next;
      });
    }
  };

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="flex h-[62vh] min-h-[360px] max-h-[620px] max-w-2xl flex-col overflow-hidden">
        <DialogHeader className="mb-0 shrink-0 pr-8">
          <div className="flex items-center justify-between gap-3">
            <DialogTitle className="truncate">{botName} 推送授权</DialogTitle>
            <Button
              size="icon"
              variant="ghost"
              className="h-8 w-8 shrink-0"
              onClick={() => void load()}
              disabled={loading}
              title="刷新推送目标"
              aria-label="刷新推送目标"
            >
              <RefreshCw className={loading ? 'animate-spin' : ''} />
            </Button>
          </div>
        </DialogHeader>

        {error && (
          <div
            role="alert"
            className="mt-4 flex shrink-0 items-start gap-2 border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
          >
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
            <span className="min-w-0 break-words">{error}</span>
          </div>
        )}

        <ScrollArea className="mt-4 min-h-0 flex-1 border-y">
          {loading && targets.length === 0 ? (
            <div className="flex min-h-56 items-center justify-center text-sm text-muted-foreground">
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              正在加载
            </div>
          ) : targets.length === 0 ? (
            <div className="flex min-h-56 items-center justify-center px-6 text-center text-sm text-muted-foreground">
              暂无已发现的推送目标
            </div>
          ) : (
            <div className="divide-y">
              {targets.map((target) => {
                const isDeleting = deleting.has(target.target_id);
                return (
                  <div
                    key={target.target_id}
                    className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 px-1 py-4"
                  >
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="truncate text-sm font-medium">{target.label}</span>
                        <Badge variant="outline">
                          {target.kind === 'group' ? '群聊' : '私聊'}
                        </Badge>
                        {target.availability === 'reply_window' ? (
                          <Badge variant="secondary">回复窗口</Badge>
                        ) : target.availability !== 'ready' ? (
                          <Badge variant="secondary">暂不可用</Badge>
                        ) : null}
                      </div>
                      <div className="mt-1 truncate text-xs text-muted-foreground">
                        最近使用：{target.last_seen_at}
                      </div>
                    </div>
                    <Button
                      size="icon"
                      variant="ghost"
                      className="h-9 w-9 text-muted-foreground hover:bg-destructive/15 hover:text-destructive"
                      onClick={() => void deleteTarget(target)}
                      disabled={isDeleting}
                      title="删除推送授权"
                      aria-label={`删除 ${target.label} 推送授权`}
                    >
                      {isDeleting ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : (
                        <Trash2 className="h-4 w-4" />
                      )}
                    </Button>
                  </div>
                );
              })}
            </div>
          )}
        </ScrollArea>
      </DialogContent>
    </Dialog>
  );
}
