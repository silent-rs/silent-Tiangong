import { useEffect, useState } from 'react';
import { api, type Webhook, type Session } from '../../api/tauri';
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
  webhook: Webhook | null;
  onClose: () => void;
}

export function WebhookFormDialog({ webhook, onClose }: Props) {
  const isEdit = !!webhook;
  const [name, setName] = useState(webhook?.name ?? '');
  const [description, setDescription] = useState(webhook?.description ?? '');
  const [secret, setSecret] = useState(webhook?.secret ?? '');
  const [payload, setPayload] = useState(webhook?.payload ?? '');
  const [sessionId, setSessionId] = useState(webhook?.session_id ?? '__none__');
  const [sessions, setSessions] = useState<Session[]>([]);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.getSessions().then(setSessions).catch(() => {});
  }, []);

  const handleSave = async () => {
    if (!name || !description || !payload) return;
    setSaving(true);
    try {
      const sid = sessionId === '__none__' ? undefined : sessionId;
      if (isEdit && webhook) {
        await api.webhookUpdate({
          id: webhook.id,
          name,
          description,
          payload,
          secret: secret || undefined,
          sessionId: sid,
        });
      } else {
        await api.webhookCreate({
          name,
          description,
          payload,
          secret: secret || undefined,
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
          <DialogTitle>{isEdit ? '编辑 Webhook' : '创建 Webhook'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <Label>名称</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="GitHub Push 触发" />
          </div>
          <div className="space-y-1.5">
            <Label>描述</Label>
            <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Webhook 描述" />
          </div>
          <div className="space-y-1.5">
            <Label>签名密钥（可选）</Label>
            <Input value={secret} onChange={(e) => setSecret(e.target.value)} placeholder="留空则不验证签名" type="password" />
            <p className="text-xs text-muted-foreground">配置后调用时需在 X-Webhook-Signature 头传入此值</p>
          </div>
          <div className="space-y-1.5">
            <Label>关联会话（可选）</Label>
            <Select value={sessionId} onValueChange={setSessionId}>
              <SelectTrigger>
                <SelectValue placeholder="不关联，自动创建新会话" />
              </SelectTrigger>
              <SelectContent>
                {/* 编辑态下若已绑定会话则禁止清空，避免丢失关联 */}
                {(!isEdit || !webhook?.session_id) && (
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
            <Button onClick={handleSave} disabled={saving || !name || !payload}>
              {saving ? '保存中...' : isEdit ? '更新' : '创建'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
