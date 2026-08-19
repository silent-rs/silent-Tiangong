import { useEffect, useState } from 'react';
import { api, type TabState } from '@/api/tauri';
import { PluginSandbox } from './PluginSandbox';

/**
 * 三方 App（extension.tab 贡献）实例内容：按贡献取入口 HTML，
 * 经标准沙箱容器（shadow/iframe）渲染；与其他 App 实例一致地
 * 挂载保活、按 isActive 显隐。
 */
export function PluginAppTabContent({ tab, isActive }: { tab: TabState; isActive: boolean }) {
  const [html, setHtml] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    setHtml('');
    setError(null);
    if (!tab.plugin_id || !tab.contribution_id) return;
    api.pluginOpenEntry(tab.plugin_id, tab.contribution_id)
      .then((page) => {
        if (active) setHtml(page);
      })
      .catch((e) => {
        if (active) setError(String(e));
      });
    return () => {
      active = false;
    };
  }, [tab.plugin_id, tab.contribution_id]);

  if (!tab.plugin_id || !tab.contribution_id) {
    return <div className="p-4 text-sm text-muted-foreground">插件实例元数据缺失。</div>;
  }
  if (error) {
    return <div className="p-4 text-sm text-muted-foreground">加载失败：{error}</div>;
  }
  if (!html) {
    return <div className="p-4 text-sm text-muted-foreground">加载中…</div>;
  }
  return (
    <div className={isActive ? 'flex h-full min-h-0 w-full flex-1 flex-col' : 'hidden'}>
      <PluginSandbox
        pluginId={tab.plugin_id}
        contributionId={tab.contribution_id}
        sandbox={tab.sandbox ?? 'shadow'}
        html={html}
      />
    </div>
  );
}
