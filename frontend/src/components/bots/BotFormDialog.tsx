import { useEffect, useState } from 'react';
import { api, type BotConfig, type ConfigFieldSchema } from '../../api/tauri';
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
  onClose: () => void;
  onSaved: () => void;
}

/**
 * bot 配置表单对话框。
 *
 * 根据 [`api.botConfigSchema`] 返回的字段定义动态渲染输入控件。
 * barcode 字段首期按 secret 输入框回退（扫码渲染逻辑预留，见 FieldType.barcode）。
 */
export function BotFormDialog({ bot, artifactId, onClose, onSaved }: Props) {
  const isEdit = !!bot;
  const { showSuccess, showError } = useToast();

  const [schema, setSchema] = useState<ConfigFieldSchema[]>([]);
  const [name, setName] = useState(bot?.name ?? '');
  const [values, setValues] = useState<Record<string, string>>({});
  const [enabled, setEnabled] = useState(bot?.enabled ?? false);
  const [saving, setSaving] = useState(false);

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

  const handleSave = async () => {
    if (!name.trim()) {
      showError('名称不能为空', '请填写 bot 名称');
      return;
    }
    setSaving(true);
    try {
      // 组装 config（按 schema 声明的字段）。
      const config: Record<string, unknown> = {};
      for (const f of schema) {
        const v = values[f.key];
        if (v !== undefined && v !== '') {
          config[f.key] = v;
        }
      }
      if (isEdit && bot) {
        await api.botUpdate(bot.id, { name: name.trim(), config });
        showSuccess('已更新', `bot "${name}" 配置已更新`);
      } else {
        const created = await api.botRegister({
          name: name.trim(),
          artifact_id: artifactId,
          config,
          enabled,
        });
        showSuccess('已注册', `bot "${created.name}" 已注册`);
      }
      onSaved();
      onClose();
    } catch (err) {
      console.error('保存 bot 失败:', err);
      showError('保存失败', String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open onOpenChange={() => onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑 Bot' : '添加 Bot'}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <Label>名称</Label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例如：我的飞书机器人"
            />
          </div>

          {schema.map((field) => (
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

          {!isEdit && (
            <div className="flex items-center gap-2">
              <Switch checked={enabled} onCheckedChange={setEnabled} />
              <Label>注册后启用</Label>
            </div>
          )}

          <div className="flex justify-end gap-2 pt-2">
            <Button variant="outline" onClick={onClose}>
              取消
            </Button>
            <Button onClick={handleSave} disabled={saving || !name.trim()}>
              {saving ? '保存中...' : isEdit ? '更新' : '创建'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/** 按字段类型渲染对应输入控件。
 *
 * barcode 类型首期回退为 secret 输入框（扫码渲染逻辑预留）。
 */
function renderField(
  field: ConfigFieldSchema,
  value: string,
  onChange: (v: string) => void,
) {
  switch (field.field_type.kind) {
    case 'secret':
    case 'barcode': // 首期回退为密码输入框
      return (
        <Input
          type="password"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={field.field_type.kind === 'barcode' ? '可扫码获取（首期手动填写）' : ''}
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
