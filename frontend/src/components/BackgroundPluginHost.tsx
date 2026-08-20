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
 * 工具接应的隐性后台实例（app.open mode=background 落地）：
 * 隐藏挂载插件 UI，只为订阅 tool.requested 让工具调用有人执行；
 * 不弹拓展区面板、不打扰用户。用户明确要求展示时由插件工具自行
 * 前台 app.open 弹出面板，与此互不影响。
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
  const workspace = useStore((state) => state.sessions.find(
    (session) => session.id === instance.sessionId,
  )?.cwd || (
    state.newConversationId === instance.sessionId
      ? state.sessionCwd || state.workspaceDir
      : state.workspaceDir
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
