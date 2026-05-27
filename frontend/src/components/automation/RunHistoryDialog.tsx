import { useCallback, useEffect, useState } from 'react';
import { api, type JobRun, type WebhookRun } from '../../api/tauri';
import { Badge } from '../ui/badge';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';

interface Props {
  type: 'job' | 'webhook';
  id: string;
  onClose: () => void;
}

type RunItem = (JobRun | WebhookRun) & {
  started_at: string;
  finished_at: string | null;
  status: string;
  result_summary: string | null;
};

const statusVariant = (status: string) => {
  switch (status) {
    case 'succeeded': return 'default' as const;
    case 'failed': return 'destructive' as const;
    case 'running': return 'secondary' as const;
    default: return 'outline' as const;
  }
};

const statusLabel = (status: string) => {
  switch (status) {
    case 'succeeded': return '成功';
    case 'failed': return '失败';
    case 'running': return '运行中';
    default: return status;
  }
};

export function RunHistoryDialog({ type, id, onClose }: Props) {
  const [runs, setRuns] = useState<RunItem[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      if (type === 'job') {
        const list = await api.jobListRuns(id, 50);
        setRuns(list);
      } else {
        const list = await api.webhookListRuns(id, 50);
        setRuns(list);
      }
    } catch (e) {
      console.error('加载执行历史失败', e);
    } finally {
      setLoading(false);
    }
  }, [type, id]);

  useEffect(() => { load(); }, [load]);

  return (
    <Dialog open onOpenChange={() => onClose()}>
      <DialogContent className="max-w-2xl max-h-[70vh]">
        <DialogHeader>
          <DialogTitle>{type === 'job' ? '任务' : 'Webhook'}执行历史</DialogTitle>
        </DialogHeader>
        <div className="overflow-auto">
          {loading ? (
            <p className="text-muted-foreground text-center py-8">加载中...</p>
          ) : runs.length === 0 ? (
            <p className="text-muted-foreground text-center py-8">暂无执行记录</p>
          ) : (
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b text-left text-muted-foreground">
                  <th className="pb-2 pr-3 font-medium">状态</th>
                  <th className="pb-2 pr-3 font-medium">开始时间</th>
                  <th className="pb-2 pr-3 font-medium">结束时间</th>
                  <th className="pb-2 font-medium">结果摘要</th>
                </tr>
              </thead>
              <tbody>
                {runs.map((run) => (
                  <tr key={run.id} className="border-b last:border-0">
                    <td className="py-2 pr-3">
                      <Badge variant={statusVariant(run.status)}>{statusLabel(run.status)}</Badge>
                    </td>
                    <td className="py-2 pr-3 whitespace-nowrap">{run.started_at}</td>
                    <td className="py-2 pr-3 whitespace-nowrap">{run.finished_at ?? '-'}</td>
                    <td className="py-2 max-w-[300px] truncate" title={run.result_summary ?? undefined}>
                      {run.result_summary ?? '-'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
