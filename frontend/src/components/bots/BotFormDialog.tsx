import { useCallback, useEffect, useMemo, useState } from 'react';
import { QRCodeSVG } from 'qrcode.react';
import { Loader2, QrCode, RefreshCw, TriangleAlert } from 'lucide-react';
import {
  api,
  type BotConfig,
  type ConfigFieldSchema,
  type QrSession,
} from '../../api/tauri';
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
import { Switch } from '../ui/switch';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../ui/dialog';
import { useToast } from '../Toast';

interface Props {
  /** 编辑时传入已有 bot；新增时传 null */
  bot: BotConfig | null;
  /** 制品 id（标识 bot 平台） */
  artifactId: string;
  /** 制品展示名称 */
  artifactName: string;
  /** 建议的 bot ID（通常为本地制品目录名），新增时预填 */
  suggestedId?: string;
  onClose: () => void;
  onSaved: () => void | Promise<void>;
}

type ProvisionState =
  | { kind: 'idle' }
  | { kind: 'starting' }
  | { kind: 'pending' }
  | { kind: 'saving' }
  | { kind: 'expired' }
  | { kind: 'error'; message: string; retrySave?: boolean };

/**
 * bot 配置表单对话框。
 *
 * 根据 [`api.botConfigSchema`] 返回的字段定义动态渲染输入控件，并为
 * barcode 字段提供统一扫码授权入口。
 */
export function BotFormDialog({
  bot,
  artifactId,
  artifactName,
  suggestedId,
  onClose,
  onSaved,
}: Props) {
  const isEdit = !!bot;
  const { showSuccess, showError } = useToast();

  const [schema, setSchema] = useState<ConfigFieldSchema[]>([]);
  const [id, setId] = useState(bot?.id ?? suggestedId ?? '');
  const [values, setValues] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [qrSession, setQrSession] = useState<QrSession | null>(null);
  const [provisionState, setProvisionState] = useState<ProvisionState>({ kind: 'idle' });

  const barcodeFields = useMemo(
    () => schema.filter((field) => field.field_type.kind === 'barcode'),
    [schema],
  );
  const configFields = useMemo(
    () => schema.filter((field) => field.field_type.kind !== 'barcode'),
    [schema],
  );
  const provisionBotId = bot?.id ?? suggestedId ?? '';
  const scanMode = provisionState.kind !== 'idle';

  // 加载该 bot 的配置字段 schema（优先已安装制品的缓存，回退到 index 预览），
  // 并用已有 bot.config / 字段默认值初始化表单。
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const fields = await api.botConfigSchema(artifactId, bot?.id);
        if (cancelled) return;
        setSchema(fields);
        const initial: Record<string, string> = {};
        for (const f of fields) {
          const existing = bot?.config?.[f.key];
          if (existing !== undefined && existing !== null) {
            initial[f.key] = String(existing);
          } else if (f.default !== undefined && f.default !== null) {
            initial[f.key] = String(f.default);
          } else {
            initial[f.key] = '';
          }
        }
        if (!cancelled) setValues(initial);
      } catch (err) {
        console.error('加载 bot 配置 schema 失败:', err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [artifactId, bot]);

  const persistBot = useCallback(
    async (mode: 'manual' | 'provision') => {
      const isProvision = mode === 'provision';
      const targetId = (isProvision ? provisionBotId : id).trim();
      if (!isEdit && !targetId) {
        showError('名称不能为空', '请填写 bot 名称');
        return;
      }

      let config: Record<string, unknown>;
      if (isProvision) {
        config = bot ? { ...bot.config } : {};
        setProvisionState({ kind: 'saving' });
      } else {
        const missingField = configFields.find(
          (field) => field.required && !(values[field.key] ?? '').trim(),
        );
        if (missingField) {
          showError('配置不完整', `请填写 ${missingField.label}`);
          return;
        }
        config = {};
        for (const field of configFields) {
          const value = values[field.key];
          if (value !== undefined && value !== '') config[field.key] = value;
        }
      }

      setSaving(true);
      try {
        if (isEdit && bot) {
          await api.botUpdate(bot.id, { config });
          showSuccess(
            isProvision ? '扫码配置完成' : '已更新',
            `“${artifactName}”配置已更新`,
          );
        } else {
          const created = await api.botRegister({
            id: targetId,
            artifact_id: artifactId,
            config,
          });
          showSuccess(
            isProvision ? '扫码配置完成' : '已注册',
            `“${artifactName || created.id}”已配置`,
          );
        }
        await onSaved();
        onClose();
      } catch (err) {
        console.error('保存 bot 失败:', err);
        const message = String(err);
        if (isProvision) {
          setProvisionState({
            kind: 'error',
            message: `扫码已完成，但自动保存失败：${message}`,
            retrySave: true,
          });
          showError('自动保存失败', message);
        } else {
          showError('保存失败', message);
        }
      } finally {
        setSaving(false);
      }
    },
    [
      artifactId,
      bot,
      configFields,
      id,
      isEdit,
      onClose,
      onSaved,
      provisionBotId,
      showError,
      showSuccess,
      values,
    ],
  );

  useEffect(() => {
    if (!qrSession || provisionState.kind !== 'pending') return;

    let cancelled = false;
    let timer: number | undefined;
    let pollInterval = qrSession.interval;

    const poll = async () => {
      if (cancelled) return;
      if (Math.floor(Date.now() / 1000) >= qrSession.expires_at) {
        setProvisionState({ kind: 'expired' });
        return;
      }

      try {
        const result = await api.botProvisionPoll(provisionBotId, qrSession);
        if (cancelled) return;

        if (result.status === 'pending') {
          pollInterval = result.retry_after ?? pollInterval;
          timer = window.setTimeout(poll, pollInterval * 1000);
          return;
        }
        if (result.status === 'expired') {
          setProvisionState({ kind: 'expired' });
          return;
        }
        if (result.status === 'error') {
          setProvisionState({ kind: 'error', message: result.message });
          return;
        }

        setQrSession(null);
        await persistBot('provision');
      } catch (err) {
        if (!cancelled) {
          setProvisionState({ kind: 'error', message: String(err) });
        }
      }
    };

    timer = window.setTimeout(poll, pollInterval * 1000);
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [persistBot, provisionBotId, provisionState.kind, qrSession]);

  const handleProvisionBegin = async () => {
    if (!provisionBotId) {
      setProvisionState({ kind: 'error', message: '未找到可执行扫码配置的 bot 制品' });
      return;
    }
    setQrSession(null);
    setProvisionState({ kind: 'starting' });
    try {
      const session = await api.botProvisionBegin(provisionBotId);
      setQrSession(session);
      setProvisionState({ kind: 'pending' });
    } catch (err) {
      setProvisionState({ kind: 'error', message: String(err) });
    }
  };

  const handleManualMode = () => {
    setQrSession(null);
    setProvisionState({ kind: 'idle' });
  };

  return (
    <Dialog open onOpenChange={(open) => !open && !saving && onClose()}>
      <DialogContent
        className="max-h-[85vh] max-w-lg overflow-y-auto"
        showCloseButton={!saving}
      >
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑 Bot' : '添加 Bot'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          {!scanMode && (
            <div className="space-y-1.5">
              <Label>
                Bot ID
                {!isEdit && <span className="text-destructive ml-1">*</span>}
              </Label>
              <Input
                value={id}
                onChange={(e) => setId(e.target.value)}
                placeholder="例如：feishu"
                disabled={isEdit}
              />
              {isEdit && (
                <p className="text-xs text-muted-foreground">创建后不可修改</p>
              )}
            </div>
          )}

          {barcodeFields.length > 0 && (
            <div className="space-y-3 border-y py-4">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="text-sm font-medium">扫码配置</div>
                  <div className="mt-0.5 text-xs text-muted-foreground">
                    扫码后由 bot 自动保存配置
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  {scanMode && provisionState.kind !== 'saving' && (
                    <Button type="button" size="sm" variant="ghost" onClick={handleManualMode}>
                      手动输入
                    </Button>
                  )}
                  <Button
                    type="button"
                    size="sm"
                    variant={scanMode ? 'outline' : 'default'}
                    onClick={() =>
                      provisionState.kind === 'error' && provisionState.retrySave
                        ? void persistBot('provision')
                        : void handleProvisionBegin()
                    }
                    disabled={
                      provisionState.kind === 'starting' || provisionState.kind === 'saving'
                    }
                  >
                    {provisionState.kind === 'starting' || provisionState.kind === 'saving' ? (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    ) : qrSession || scanMode ? (
                      <RefreshCw className="mr-2 h-4 w-4" />
                    ) : (
                      <QrCode className="mr-2 h-4 w-4" />
                    )}
                    {provisionState.kind === 'starting'
                      ? '生成中...'
                      : provisionState.kind === 'saving'
                        ? '保存中...'
                        : provisionState.kind === 'error' && provisionState.retrySave
                          ? '重试保存'
                          : scanMode
                            ? '重新生成'
                            : '扫码配置'}
                  </Button>
                </div>
              </div>

              {qrSession && provisionState.kind === 'pending' && (
                <div className="flex flex-col items-center gap-3 pt-1">
                  <div className="flex h-[208px] w-[208px] items-center justify-center rounded-md bg-white p-3">
                    <QRCodeSVG
                      value={qrSession.qr_url}
                      size={184}
                      bgColor="#ffffff"
                      fgColor="#111111"
                      level="M"
                    />
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    等待扫码授权
                  </div>
                </div>
              )}

              {provisionState.kind === 'saving' && (
                <div className="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  配置处理完成，正在保存
                </div>
              )}

              {(provisionState.kind === 'expired' || provisionState.kind === 'error') && (
                <div className="flex items-start gap-2 text-sm text-destructive">
                  <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
                  <span>
                    {provisionState.kind === 'expired'
                      ? '二维码已过期，请重新生成'
                      : provisionState.message}
                  </span>
                </div>
              )}
            </div>
          )}

          {!scanMode &&
            configFields.map((field) => (
              <div key={field.key} className="space-y-1.5">
                <Label>
                  {field.label}
                  {field.required && <span className="text-destructive ml-1">*</span>}
                </Label>
                {renderField(field, values[field.key] ?? '', (v) =>
                  setValues((prev) => ({ ...prev, [field.key]: v })),
                )}
                {field.help && (
                  <p className="text-xs text-muted-foreground">{field.help}</p>
                )}
              </div>
            ))}

          <div className="flex justify-end gap-2 pt-2">
            <Button variant="outline" onClick={onClose} disabled={saving}>
              取消
            </Button>
            {!scanMode && (
              <Button
                onClick={() => void persistBot('manual')}
                disabled={saving || (!isEdit && !id.trim())}
              >
                {saving ? '保存中...' : isEdit ? '更新' : '创建'}
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** 按字段类型渲染对应输入控件。 */
function renderField(
  field: ConfigFieldSchema,
  value: string,
  onChange: (v: string) => void,
) {
  switch (field.field_type.kind) {
    case 'barcode':
      return null;
    case 'secret':
      return (
        <Input
          type="password"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      );
    case 'boolean':
      return (
        <Switch checked={value === 'true'} onCheckedChange={(c) => onChange(c ? 'true' : 'false')} />
      );
    case 'select':
      return (
        <Select value={value} onValueChange={onChange}>
          <SelectTrigger>
            <SelectValue placeholder="请选择" />
          </SelectTrigger>
          <SelectContent>
            {field.field_type.options.map((opt) => (
              <SelectItem key={opt} value={opt}>
                {opt}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      );
    case 'string':
    default:
      return <Input value={value} onChange={(e) => onChange(e.target.value)} />;
  }
}
