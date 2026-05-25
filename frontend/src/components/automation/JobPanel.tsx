import { useCallback, useEffect, useState } from 'react';
import { api, type Job } from '../../api/tauri';
import { Button } from '../ui/button';
import { Switch } from '../ui/switch';
import { Badge } from '../ui/badge';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '../ui/tooltip';
import { JobFormDialog } from './JobFormDialog';
import { RunHistoryDialog } from './RunHistoryDialog';

interface JobPanelProps {
  serverRunning: boolean;
}

export function JobPanel({ serverRunning }: JobPanelProps) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [loading, setLoading] = useState(true);
  const [showForm, setShowForm] = useState(false);
  const [editingJob, setEditingJob] = useState<Job | null>(null);
  const [runHistoryJobId, setRunHistoryJobId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const list = await api.jobList();
      setJobs(list);
    } catch (e) {
      console.error('加载定时任务失败', e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleToggle = async (job: Job) => {
    try {
      await api.jobUpdate({ id: job.id, enabled: !job.enabled });
      await load();
    } catch (e) {
      console.error('切换状态失败', e);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await api.jobDelete(id);
      await load();
    } catch (e) {
      console.error('删除失败', e);
    }
  };

  const handleTrigger = async (id: string) => {
    try {
      await api.jobTrigger(id);
    } catch (e) {
      console.error('触发失败', e);
    }
  };

  const handleEdit = (job: Job) => {
    setEditingJob(job);
    setShowForm(true);
  };

  const handleFormClose = () => {
    setShowForm(false);
    setEditingJob(null);
    load();
  };

  if (loading) {
    return <div className="p-6 text-muted-foreground">加载中...</div>;
  }

  return (
    <TooltipProvider delayDuration={300}>
      <div className="p-6 space-y-4">
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            共 {jobs.length} 个定时任务
          </p>
          <Button size="sm" onClick={() => { setEditingJob(null); setShowForm(true); }}>
            创建任务
          </Button>
        </div>

        {jobs.length === 0 ? (
          <div className="text-center py-12 text-muted-foreground">
            <p className="text-lg">暂无定时任务</p>
            <p className="text-sm mt-1">点击「创建任务」添加一个新的 Cron 定时任务</p>
          </div>
        ) : (
          <div className="space-y-2">
            {jobs.map((job) => (
              <JobRow
                key={job.id}
                job={job}
                active={serverRunning}
                onToggle={() => handleToggle(job)}
                onTrigger={() => handleTrigger(job.id)}
                onHistory={() => setRunHistoryJobId(job.id)}
                onEdit={() => handleEdit(job)}
                onDelete={() => handleDelete(job.id)}
              />
            ))}
          </div>
        )}

        {showForm && (
          <JobFormDialog job={editingJob} onClose={handleFormClose} />
        )}

        {runHistoryJobId && (
          <RunHistoryDialog
            type="job"
            id={runHistoryJobId}
            onClose={() => setRunHistoryJobId(null)}
          />
        )}
      </div>
    </TooltipProvider>
  );
}

interface JobRowProps {
  job: Job;
  active: boolean;
  onToggle: () => void;
  onTrigger: () => void;
  onHistory: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

function JobRow({ job, active, onToggle, onTrigger, onHistory, onEdit, onDelete }: JobRowProps) {
  const row = (
    <div
      className={`flex items-center gap-3 p-3 rounded-lg border transition-colors ${
        active
          ? 'bg-card hover:bg-accent/50'
          : 'bg-card/50 opacity-60'
      }`}
    >
      <Switch
        checked={job.enabled}
        onCheckedChange={onToggle}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-medium truncate">{job.name}</span>
          <Badge variant="outline" className="text-xs font-mono">
            {job.schedule}
          </Badge>
          {!active && (
            <Badge variant="secondary" className="text-xs text-amber-600 dark:text-amber-400">
              未激活
            </Badge>
          )}
        </div>
        <p className="text-sm text-muted-foreground truncate">{job.description}</p>
      </div>
      <div className="flex items-center gap-1 shrink-0">
        <Button variant="ghost" size="sm" disabled={!active} onClick={onTrigger}>
          触发
        </Button>
        <Button variant="ghost" size="sm" onClick={onHistory}>
          历史
        </Button>
        <Button variant="ghost" size="sm" onClick={onEdit}>
          编辑
        </Button>
        <Button variant="ghost" size="sm" className="text-destructive" onClick={onDelete}>
          删除
        </Button>
      </div>
    </div>
  );

  if (active) {
    return row;
  }

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        {row}
      </TooltipTrigger>
      <TooltipContent side="top">
        Server 未运行，定时任务处于未激活状态。请在「Server」选项卡中启动 Server。
      </TooltipContent>
    </Tooltip>
  );
}
