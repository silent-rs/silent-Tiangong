import { useCallback, useEffect, useState } from 'react';
import { api, type SlotContributionEntry } from '@/api/tauri';
import { PluginSandbox } from './PluginSandbox';

interface SessionInputPluginHostProps {
  slot: 'session.input-action' | 'session.before-input' | 'session.after-input';
}

/** 会话输入区 Slot 宿主：挂载已安装插件声明的输入辅助贡献。 */
export function SessionInputPluginHost({ slot }: SessionInputPluginHostProps) {
  const [items, setItems] = useState<Array<SlotContributionEntry & { html: string }>>([]);

  const refresh = useCallback(async () => {
    const contributions = await api.listSlotContributions(slot);
    const loaded = await Promise.all(contributions.map(async (item) => ({
      ...item,
      html: item.source === 'manifest'
        ? await api.pluginOpenEntry(item.plugin_id, item.contribution_id)
        : await api.pluginOpenView(item.plugin_id, item.contribution_id),
    })));
    setItems(loaded);
  }, [slot]);

  useEffect(() => {
    let disposed = false;
    const reload = () => {
      void refresh().catch((error) => {
        if (!disposed) console.warn(`[session-input-plugin] 加载 ${slot} 失败`, error);
      });
    };
    reload();
    window.addEventListener('tiangong:plugins-changed', reload);
    return () => {
      disposed = true;
      window.removeEventListener('tiangong:plugins-changed', reload);
    };
  }, [refresh, slot]);

  if (items.length === 0) return null;
  return (
    <>
      {items.map((item) => (
        <PluginSandbox
          key={`${item.plugin_id}:${item.contribution_id}`}
          pluginId={item.plugin_id}
          contributionId={item.contribution_id}
          sandbox={item.sandbox}
          html={item.html}
          className={slot === 'session.input-action' ? 'h-8 w-8 shrink-0 overflow-hidden' : undefined}
        />
      ))}
    </>
  );
}
