import { useEffect, useMemo, useState } from 'react';
import { api, type Job, type Session } from '../../api/tauri';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { Label } from '../ui/label';
import { Badge } from '../ui/badge';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../ui/select';
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '../ui/tabs';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import {
  buildSchedule,
  DEFAULT_SIMPLE,
  humanizeCron,
  nextRuns,
  tryParseToSimple,
  validateCron,
  WEEKDAY_OPTIONS,
  type SimpleSchedule,
} from '../../lib/cron';

interface Props {
  job: Job | null;
  onClose: () => void;
}

type Mode = 'simple' | 'cron';

export function JobFormDialog({ job, onClose }: Props) {
  const isEdit = !!job;
  const [name, setName] = useState(job?.name ?? '');
  const [description, setDescription] = useState(job?.description ?? '');
  const [payload, setPayload] = useState(job?.payload ?? '');
  const [sessionId, setSessionId] = useState(job?.session_id ?? '__none__');
  const [sessions, setSessions] = useState<Session[]>([]);
  const [saving, setSaving] = useState(false);

  // schedule 双模式：编辑态优先尝试回填到简单模式，否则进 cron 模式
  const initialSimple = tryParseToSimple(job?.schedule);
  const [mode, setMode] = useState<Mode>(initialSimple ? 'simple' : 'cron');
  const [simple, setSimple] = useState<SimpleSchedule>(initialSimple ?? DEFAULT_SIMPLE);
  const [cronExpr, setCronExpr] = useState(job?.schedule ?? '0 0 9 * * *');

  useEffect(() => {
    api.getSessions().then(setSessions).catch(() => {});
  }, []);

  // 最终提交的 6 字段表达式：simple 模式由字段合成，cron 模式直接取输入
  const schedule = useMemo(
    () => (mode === 'simple' ? buildSchedule(simple) : cronExpr),
    [mode, simple, cronExpr],
  );

  const cronValid = useMemo(() => validateCron(schedule), [schedule]);

  const handleSave = async () => {
    if (!name || !description || !payload || !cronValid.ok) return;
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
          sessionId: sid,
        });
      } else {
        await api.jobCreate({
          name,
          description,
          schedule,
          payload,
          sessionId: sid,
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

          {/* 定时设置：简单 / Cron 双模式 */}
          <div className="space-y-2">
            <Tabs value={mode} onValueChange={(v) => setMode(v as Mode)}>
              <TabsList>
                <TabsTrigger value="simple">简单</TabsTrigger>
                <TabsTrigger value="cron">Cron 表达式</TabsTrigger>
              </TabsList>

              {/* 简单模式：分钟 / 小时 / 星期 */}
              <TabsContent value="simple" className="space-y-2 pt-2">
                <div className="grid grid-cols-3 gap-2">
                  <div className="space-y-1">
                    <Label className="text-xs text-muted-foreground">分钟</Label>
                    <Input
                      type="number"
                      min={0}
                      max={59}
                      value={simple.minute}
                      onChange={(e) =>
                        setSimple((s) => ({ ...s, minute: Number(e.target.value) || 0 }))
                      }
                    />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs text-muted-foreground">小时</Label>
                    <Input
                      type="number"
                      min={0}
                      max={23}
                      value={simple.hour}
                      onChange={(e) =>
                        setSimple((s) => ({ ...s, hour: Number(e.target.value) || 0 }))
                      }
                    />
                  </div>
                  <div className="space-y-1">
                    <Label className="text-xs text-muted-foreground">星期</Label>
                    <Select
                      value={simple.weekday}
                      onValueChange={(v) => setSimple((s) => ({ ...s, weekday: v }))}
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {WEEKDAY_OPTIONS.map((o) => (
                          <SelectItem key={o.value} value={o.value}>
                            {o.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </div>
                <p className="font-mono text-xs text-muted-foreground">合成表达式：{schedule}</p>
              </TabsContent>

              {/* Cron 模式：手填 6 字段 */}
              <TabsContent value="cron" className="space-y-1.5 pt-2">
                <Input
                  value={cronExpr}
                  onChange={(e) => setCronExpr(e.target.value)}
                  placeholder="0 0 9 * * *"
                  className="font-mono"
                />
                <p className="text-xs text-muted-foreground">
                  格式：秒 分 时 日 月 周（6 字段）。例如每天 9 点：
                  <code className="mx-1 rounded bg-muted px-1">0 0 9 * * *</code>
                </p>
              </TabsContent>
            </Tabs>

            {/* 校验状态 + 预览（两模式共用） */}
            <SchedulePreview schedule={schedule} valid={cronValid} />
          </div>

          <div className="space-y-1.5">
            <Label>关联会话（可选）</Label>
            <Select value={sessionId} onValueChange={setSessionId}>
              <SelectTrigger>
                <SelectValue placeholder="不关联，自动创建新会话" />
              </SelectTrigger>
              <SelectContent>
                {/* 编辑态下若已绑定会话则禁止清空，避免丢失关联 */}
                {(!isEdit || !job?.session_id) && (
                  <SelectItem value="__none__">不关联，自动创建新会话</SelectItem>
                )}
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
            <Button onClick={handleSave} disabled={saving || !name || !payload || !cronValid.ok}>
              {saving ? '保存中...' : isEdit ? '更新' : '创建'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** 校验状态徽章 + 下次触发时间预览（两模式共用）。 */
function SchedulePreview({
  schedule,
  valid,
}: {
  schedule: string;
  valid: { ok: boolean; error?: string };
}) {
  // 合法时计算人话描述与接下来 3 次触发时间
  const human = useMemo(() => (valid.ok ? humanizeCron(schedule) : null), [schedule, valid.ok]);
  const runs = useMemo(() => (valid.ok ? nextRuns(schedule, 3) : null), [schedule, valid.ok]);

  if (!valid.ok) {
    return (
      <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2">
        <Badge variant="destructive">无效</Badge>
        <p className="text-xs text-destructive">{valid.error}</p>
      </div>
    );
  }

  return (
    <div className="space-y-1.5 rounded-md border bg-muted/30 px-3 py-2">
      <div className="flex items-center gap-2">
        <Badge variant="secondary">有效</Badge>
        {human && <span className="text-xs text-muted-foreground">{human}</span>}
      </div>
      {runs && runs.length > 0 && (
        <div className="space-y-0.5 text-xs text-muted-foreground">
          <p>下次触发：{formatLocal(runs[0])}</p>
          {runs.length > 1 && (
            <p>
              接下来：
              {runs.slice(1).map((d, i) => (
                <span key={i}>
                  {i > 0 && '、'}{relativeFromNow(d, runs[0])}
                </span>
              ))}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** 本地日期时间格式化：YYYY-MM-DD HH:mm（周几）。 */
function formatLocal(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  const wd = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'][d.getDay()];
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())} ${wd}`;
}

/**
 * 相对参考时间的人话描述：例如距参考点 +1 天 →「约 1 天后」。
 * 用于「接下来」列表，比绝对时间更易读。
 */
function relativeFromNow(d: Date, ref: Date): string {
  const diffMs = d.getTime() - ref.getTime();
  const mins = Math.round(diffMs / 60000);
  if (mins < 60) return `约 ${mins} 分钟后`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `约 ${hours} 小时后`;
  const days = Math.round(hours / 24);
  return `约 ${days} 天后`;
}
