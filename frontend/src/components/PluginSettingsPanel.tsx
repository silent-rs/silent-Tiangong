import { useEffect, useState } from 'react';
import {
  api,
  type PluginContributionEntry,
  type PluginConfigField,
} from '../api/tauri';

/**
 * 插件设置面板：动态加载 WASM 插件贡献的设置入口，
 * 用后端声明的 schema 驱动表单渲染（参照 bot 配置表单范式）。
 */
export function PluginSettingsPanel() {
  const [contributions, setContributions] = useState<PluginContributionEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api.listPluginContributions()
      .then((entries) => {
        setContributions(entries);
        if (entries.length > 0) {
          setSelected(entries[0].plugin_id);
        }
      })
      .catch(() => setContributions([]))
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return <div className="p-4 text-sm text-muted-foreground">加载插件设置…</div>;
  }

  if (contributions.length === 0) {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        暂无已加载的 WASM 插件设置页。
      </div>
    );
  }

  return (
    <div className="flex h-full">
      {/* 左侧：插件设置入口列表 */}
      <div className="w-48 border-r pr-2 space-y-1">
        {contributions.map((entry) => (
          <button
            key={`${entry.plugin_id}:${entry.contribution_id}`}
            onClick={() => setSelected(entry.plugin_id)}
            className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
              selected === entry.plugin_id
                ? 'bg-accent text-accent-foreground'
                : 'hover:bg-accent/50'
            }`}
          >
            {entry.title}
          </button>
        ))}
      </div>

      {/* 右侧：选中插件的配置表单 */}
      <div className="flex-1 pl-4 overflow-auto">
        {selected && <PluginConfigForm pluginId={selected} />}
      </div>
    </div>
  );
}

/** 单个插件的配置表单（schema 驱动）。 */
function PluginConfigForm({ pluginId }: { pluginId: string }) {
  const [fields, setFields] = useState<PluginConfigField[]>([]);
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    Promise.all([
      api.getPluginConfigSchema(pluginId),
      api.getPluginConfig(pluginId),
    ])
      .then(([schemaJson, configJson]) => {
        try {
          const schema = JSON.parse(schemaJson);
          setFields(schema.fields || []);
        } catch { setFields([]); }
        try {
          setValues(JSON.parse(configJson));
        } catch { setValues({}); }
      })
      .catch(() => { setFields([]); setValues({}); });
  }, [pluginId]);

  const handleSave = () => {
    setSaving(true);
    api.setPluginConfig(pluginId, JSON.stringify(values))
      .then(() => { setSaved(true); setTimeout(() => setSaved(false), 2000); })
      .catch(() => {})
      .finally(() => setSaving(false));
  };

  if (fields.length === 0) {
    return <div className="p-4 text-sm text-muted-foreground">该插件无可配置项。</div>;
  }

  return (
    <div className="space-y-4 p-2">
      {fields.map((field) => (
        <div key={field.key} className="space-y-1">
          <label className="text-sm font-medium">
            {field.label}
            {field.required && <span className="text-destructive ml-1">*</span>}
          </label>
          <input
            type={field.type === 'secret' ? 'password' : field.type === 'integer' ? 'number' : 'text'}
            value={String(values[field.key] ?? field.default ?? '')}
            onChange={(e) => {
              const val = field.type === 'integer' ? Number(e.target.value) : e.target.value;
              setValues({ ...values, [field.key]: val });
            }}
            className="w-full px-3 py-2 rounded-md border bg-background text-sm"
            placeholder={field.help || ''}
          />
          {field.help && (
            <p className="text-xs text-muted-foreground">{field.help}</p>
          )}
        </div>
      ))}
      <div className="flex items-center gap-3 pt-2">
        <button
          onClick={handleSave}
          disabled={saving}
          className="px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm disabled:opacity-50"
        >
          {saving ? '保存中…' : '保存'}
        </button>
        {saved && <span className="text-sm text-green-600">已保存</span>}
      </div>
    </div>
  );
}
