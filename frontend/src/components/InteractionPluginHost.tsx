import { useEffect, useState } from 'react';
import { api, type SlotContributionEntry } from '@/api/tauri';
import { PluginSandbox } from './PluginSandbox';
import { useStore } from '@/store/useStore';

/**
 * 交互处理器插件挂载点。宿主不渲染审批表单，只挂载第一个声明
 * `session.interaction` 的插件贡献；请求和闭合状态均通过 Bridge 事件送达插件。
 */
export function InteractionPluginHost() {
  const runStatus = useStore((state) => state.runStatus);
  const [handler, setHandler] = useState<SlotContributionEntry | null>(null);
  const [html, setHtml] = useState('');

  useEffect(() => {
    let cancelled = false;
    void api.listSlotContributions('session.interaction').then(async (items) => {
      const selected = items[0] ?? null;
      if (!selected || cancelled) return;
      const view = await api.pluginOpenView(selected.plugin_id, selected.contribution_id);
      if (!cancelled) {
        setHandler(selected);
        setHtml(view);
      }
    }).catch((error) => console.warn('[interaction-plugin] 加载处理器失败', error));
    return () => { cancelled = true; };
  }, []);

  if (!handler || !html) {
    return runStatus === 'waiting_approval' ? (
      <div className="flex justify-start text-xs text-muted-foreground">
        正在等待交互处理器；若未安装处理器，请求将在截止时间后安全取消。
      </div>
    ) : null;
  }

  return (
    <div className={`${runStatus === 'waiting_approval' ? '' : 'hidden'} min-h-[120px] w-full max-w-2xl overflow-hidden rounded-lg border bg-card`}>
      <PluginSandbox
        pluginId={handler.plugin_id}
        contributionId={handler.contribution_id}
        sandbox={handler.sandbox}
        html={html}
      />
    </div>
  );
}
