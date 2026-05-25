import { useEffect, useState } from 'react';
import { api, type Job, type Session } from '../../api/tauri';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { Label } from '../ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../ui/select';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';

interface Props {
  job: Job | null;
  onClose: () => void;
}

export function JobFormDialog({ job, onClose }: Props) {
  const isEdit = !!job;
  const [name, setName] = useState(job?.name ?? '');
  const [description, setDescription] = useState(job?.description ?? '');
  const [schedule, setSchedule] = useState(job?.schedule ?? '');
  const [payload, setPayload] = useState(job?.payload ?? '');
  const [sessionId, setSessionId] = useState(job?.session_id ?? '__none__');
  const [sessions, setSessions] = useState<Session[]>([]);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.getSessions().then(setSessions).catch(() => {});
  }, []);

  const handleSave = async () => {
    if (!name || !description || !schedule || !payload) return;
    setSaving(true);
    try {
      const sid = sessionId === '__none__' ? undefined : sessionId;
      if (isEdit && job) {
        await api.jobUpdate({
          id: job.id,
          name,
          description,
          schedule,
          payload,
          session_id: sid,
        });
      } else {
        await api.jobCreate({
          name,
          description,
          schedule,
          payload,
          session_id: sid,
          enabled: true,
        });
      }
      onClose();
    } catch (e) {
      console.error('保存失败', e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open onOpenChange={() => onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑定时任务' : '创建定时任务'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <Label>名称</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="每日站会提醒" />
          </div>
          <div className="space-y-1.5">
            <Label>描述</Label>
            <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="任务描述" />
          </div>
          <div className="space-y-1.5">
            <Label>Cron 表达式</Label>
            <Input value={schedule} onChange={(e) => setSchedule(e.target.value)} placeholder="0 9 * * *" className="font-mono" />
            <p className="text-xs text-muted-foreground">格式：分 时 日 月 周。例如每天 9 点：0 9 * * *</p>
          </div>
          <div className="space-y-1.5">
            <Label>关联会话（可选）</Label>
            <Select value={sessionId} onValueChange={setSessionId}>
              <SelectTrigger>
                <SelectValue placeholder="不关联，自动创建新会话" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">不关联，自动创建新会话</SelectItem>
                {sessions.map((s) => (
                  <SelectItem key={s.id} value={s.id}>
                    {s.title || s.id}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label>任务内容</Label>
            <textarea
              className="flex min-h-[100px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              value={payload}
              onChange={(e) => setPayload(e.target.value)}
              placeholder="触发时发送给 AI 的任务描述"
            />
          </div>
          <div className="flex justify-end gap-2 pt-2">
            <Button variant="outline" onClick={onClose}>取消</Button>
            <Button onClick={handleSave} disabled={saving || !name || !schedule || !payload}>
              {saving ? '保存中...' : isEdit ? '更新' : '创建'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
