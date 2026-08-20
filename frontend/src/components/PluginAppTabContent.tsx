import { useEffect, useState } from 'react';
import { api, type TabState } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { PluginSandbox } from './PluginSandbox';

/**
 * 三方 App（extension.tab 贡献）实例内容：按贡献取入口 HTML，
 * 经标准沙箱容器（shadow/iframe）渲染；与其他 App 实例一致地
 * 挂载保活、按 isActive 显隐。
 *
 * 会话上下文（sessionId + workspace）随当前活跃会话注入：终端等
 * 插件以 workspace 作为默认初始目录，与内置终端面板行为一致。
 */
interface PluginAppTabContentProps {
  tab: TabState;
  isActive: boolean;
  sessionId?: string | null;
  onRequestNew?: () => void;
}

export function PluginAppTabContent({
  tab,
  isActive,
  sessionId,
  onRequestNew,
}: PluginAppTabContentProps) {
  const [html, setHtml] = useState('');
  const [error, setError] = useState<string | null>(null);
  const activeSessionId = useStore((s) => s.activeSessionId);
  const sessionCwd = useStore((s) => s.sessionCwd);
  const workspaceDir = useStore((s) => s.workspaceDir);
  const containerClassName = isActive
    ? 'flex h-full min-h-0 w-full flex-1 flex-col'
    : 'hidden';

  useEffect(() => {
    if (!isActive || !onRequestNew) return;
    const handleRequestNew = (event: Event) => {
      const detail = (event as CustomEvent<{
        plugin_id?: string;
        contribution_id?: string;
      }>).detail;
      if (
        detail?.plugin_id === tab.plugin_id
        && detail?.contribution_id === tab.contribution_id
      ) {
        onRequestNew();
      }
    };
    window.addEventListener('tiangong:plugin-request-new', handleRequestNew);
    return () => window.removeEventListener('tiangong:plugin-request-new', handleRequestNew);
  }, [isActive, onRequestNew, tab.contribution_id, tab.plugin_id]);

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
    return (
      <div className={`${containerClassName} p-4 text-sm text-muted-foreground`}>
        插件实例元数据缺失。
      </div>
    );
  }
  if (error) {
    return (
      <div className={`${containerClassName} p-4 text-sm text-muted-foreground`}>
        加载失败：{error}
      </div>
    );
  }
  if (!html) {
    return (
      <div className={`${containerClassName} p-4 text-sm text-muted-foreground`}>
        加载中…
      </div>
    );
  }
  return (
    <div className={containerClassName}>
      <PluginSandbox
        pluginId={tab.plugin_id}
        contributionId={tab.contribution_id}
        sandbox={tab.sandbox ?? 'shadow'}
        html={html}
        sessionId={sessionId ?? activeSessionId ?? null}
        workspace={sessionCwd || workspaceDir || null}
        instanceId={tab.id}
        visible={isActive}
      />
    </div>
  );
}
