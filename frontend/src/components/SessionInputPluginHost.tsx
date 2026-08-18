import { useEffect, useState } from 'react';
import { api, type SlotContributionEntry } from '@/api/tauri';
import { PluginSandbox } from './PluginSandbox';

interface SessionInputPluginHostProps {
  slot: 'session.input-action' | 'session.before-input' | 'session.after-input';
}

/** 会话输入区 Slot 宿主：挂载已安装插件声明的输入辅助贡献。 */
export function SessionInputPluginHost({ slot }: SessionInputPluginHostProps) {
  const [items, setItems] = useState<Array<SlotContributionEntry & { html: string }>>([]);

  useEffect(() => {
    let cancelled = false;
    void api.listSlotContributions(slot).then(async (contributions) => {
      const loaded = await Promise.all(contributions.map(async (item) => ({
        ...item,
        html: item.source === 'manifest'
          ? await api.pluginOpenEntry(item.plugin_id, item.contribution_id)
          : await api.pluginOpenView(item.plugin_id, item.contribution_id),
      })));
      if (!cancelled) setItems(loaded);
    }).catch((error) => console.warn(`[session-input-plugin] 加载 ${slot} 失败`, error));
    return () => { cancelled = true; };
  }, [slot]);

  if (items.length === 0) return null;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-1.5">
      {items.map((item) => (
        <div key={`${item.plugin_id}:${item.contribution_id}`} className="min-w-0">
          <PluginSandbox
            pluginId={item.plugin_id}
            contributionId={item.contribution_id}
            sandbox={item.sandbox}
            html={item.html}
          />
        </div>
      ))}
    </div>
  );
}
