import { useCallback, useEffect, useState } from 'react';
import { api, type Webhook } from '../../api/tauri';
import { Button } from '../ui/button';
import { Switch } from '../ui/switch';
import { Badge } from '../ui/badge';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '../ui/tooltip';
import { WebhookFormDialog } from './WebhookFormDialog';
import { RunHistoryDialog } from './RunHistoryDialog';

interface WebhookPanelProps {
  serverRunning: boolean;
}

export function WebhookPanel({ serverRunning }: WebhookPanelProps) {
  const [webhooks, setWebhooks] = useState<Webhook[]>([]);
  const [loading, setLoading] = useState(true);
  const [showForm, setShowForm] = useState(false);
  const [editingWebhook, setEditingWebhook] = useState<Webhook | null>(null);
  const [runHistoryId, setRunHistoryId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const list = await api.webhookList();
      setWebhooks(list);
    } catch (e) {
      console.error('加载 Webhook 失败', e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleToggle = async (w: Webhook) => {
    try {
      await api.webhookUpdate({ id: w.id, enabled: !w.enabled });
      await load();
    } catch (e) {
      console.error('切换状态失败', e);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await api.webhookDelete(id);
      await load();
    } catch (e) {
      console.error('删除失败', e);
    }
  };

  const handleTrigger = async (id: string) => {
    try {
      await api.webhookTrigger(id);
    } catch (e) {
      console.error('触发失败', e);
    }
  };

  const handleEdit = (w: Webhook) => {
    setEditingWebhook(w);
    setShowForm(true);
  };

  const handleFormClose = () => {
    setShowForm(false);
    setEditingWebhook(null);
    load();
  };

  const invokeUrl = (id: string) => `POST /api/v1/webhooks/${id}/invoke`;

  if (loading) {
    return <div className="p-6 text-muted-foreground">加载中...</div>;
  }

  return (
    <TooltipProvider delayDuration={300}>
      <div className="p-6 space-y-4">
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            共 {webhooks.length} 个 Webhook
          </p>
          <Button size="sm" onClick={() => { setEditingWebhook(null); setShowForm(true); }}>
            创建 Webhook
          </Button>
        </div>

        {webhooks.length === 0 ? (
          <div className="text-center py-12 text-muted-foreground">
            <p className="text-lg">暂无 Webhook</p>
            <p className="text-sm mt-1">点击「创建 Webhook」添加一个 HTTP 触发端点</p>
          </div>
        ) : (
          <div className="space-y-2">
            {webhooks.map((w) => (
              <WebhookRow
                key={w.id}
                webhook={w}
                active={serverRunning}
                invokeUrl={invokeUrl(w.id)}
                onToggle={() => handleToggle(w)}
                onTrigger={() => handleTrigger(w.id)}
                onHistory={() => setRunHistoryId(w.id)}
                onEdit={() => handleEdit(w)}
                onDelete={() => handleDelete(w.id)}
              />
            ))}
          </div>
        )}

        {showForm && (
          <WebhookFormDialog webhook={editingWebhook} onClose={handleFormClose} />
        )}

        {runHistoryId && (
          <RunHistoryDialog
            type="webhook"
            id={runHistoryId}
            onClose={() => setRunHistoryId(null)}
          />
        )}
      </div>
    </TooltipProvider>
  );
}

interface WebhookRowProps {
  webhook: Webhook;
  active: boolean;
  invokeUrl: string;
  onToggle: () => void;
  onTrigger: () => void;
  onHistory: () => void;
  onEdit: () => void;
  onDelete: () => void;
}

function WebhookRow({ webhook, active, invokeUrl, onToggle, onTrigger, onHistory, onEdit, onDelete }: WebhookRowProps) {
  const row = (
    <div
      className={`flex items-center gap-3 p-3 rounded-lg border transition-colors ${
        active
          ? 'bg-card hover:bg-accent/50'
          : 'bg-card/50 opacity-60'
      }`}
    >
      <Switch checked={webhook.enabled} onCheckedChange={onToggle} />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-medium truncate">{webhook.name}</span>
          {webhook.secret ? (
            <Badge variant="default" className="text-xs">已设密钥</Badge>
          ) : (
            <Badge variant="secondary" className="text-xs">无密钥</Badge>
          )}
          {!active && (
            <Badge variant="secondary" className="text-xs text-amber-600 dark:text-amber-400">
              未激活
            </Badge>
          )}
        </div>
        <p className="text-xs text-muted-foreground font-mono truncate mt-0.5">
          {invokeUrl}
        </p>
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
        Server 未运行，Webhook 处于未激活状态。请在「Server」选项卡中启动 Server。
      </TooltipContent>
    </Tooltip>
  );
}
