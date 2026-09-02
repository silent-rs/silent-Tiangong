import { useEffect, useState } from 'react';
import { api, type SandboxKind } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { PluginSandbox } from './PluginSandbox';

export interface BackgroundPluginInstance {
  pluginId: string;
  contributionId: string;
  sandbox: SandboxKind;
  /** 发起工具调用的会话（实例跟会话绑定，支持多会话并存）。 */
  sessionId: string;
}

/**
 * 后台会话（Sub Agent/Bot 等）工具接应的隐性挂载：隐藏挂载插件 UI，
 * 只为订阅 tool.requested 让其工具有人执行；不弹拓展区面板、不进入
 * 前台标签与绿点。当前会话的工具拉起一律建立可见标签（MainApp
 * app:open_plugin 分流），此处仅兜底无法进入标签栏的后台会话。
 */
export function BackgroundPluginHost({
  instances,
}: {
  instances: BackgroundPluginInstance[];
}) {
  return (
    <div className="hidden" aria-hidden="true">
      {instances.map((instance) => (
        <BackgroundInstance
          key={`${instance.pluginId}:${instance.contributionId}:${instance.sessionId}`}
          instance={instance}
        />
      ))}
    </div>
  );
}

function BackgroundInstance({ instance }: { instance: BackgroundPluginInstance }) {
  const [html, setHtml] = useState('');
  // workspace 仅用于展示给插件；Sidecar 权限不消费它。已持久化会话若
  // 未出现在前端列表中不回退当前可见会话的全局工作区，避免造成误导。
  const workspace = useStore((state) => state.sessions.find(
    (session) => session.id === instance.sessionId,
  )?.cwd || (
    state.newConversationId === instance.sessionId
      ? state.sessionCwd || state.workspaceDir
      : ''
  ));

  useEffect(() => {
    let active = true;
    setHtml('');
    api
      .pluginOpenEntry(instance.pluginId, instance.contributionId)
      .then((page) => {
        if (active) setHtml(page);
      })
      .catch((error) => console.error('后台挂载插件实例失败:', error));
    return () => {
      active = false;
    };
  }, [instance.pluginId, instance.contributionId]);

  if (!html) return null;
  return (
    <PluginSandbox
      pluginId={instance.pluginId}
      contributionId={instance.contributionId}
      sandbox={instance.sandbox}
      html={html}
      sessionId={instance.sessionId}
      workspace={workspace}
      instanceId={`bg-${instance.pluginId}`}
      visible={false}
    />
  );
}
