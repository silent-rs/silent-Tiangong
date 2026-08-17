import { useEffect, useRef, useCallback, useMemo } from 'react';
import { api } from '../api/tauri';
import { useResolvedTheme } from '../hooks/useTheme';
import { usePluginMask } from '../hooks/usePluginMask';
import { hostContext } from './pluginHostContext';

const pluginCallQueues = new Map<string, Promise<void>>();

/**
 * 插件 iframe 容器 + postMessage 桥接（sandbox: "iframe" 强隔离模式）。
 *
 * iframe 内的 JS 经 window.parent.postMessage({ type: 'plugin_call', ... }) 发消息，
 * 本组件收到后调 api.bridgeCall 经宿主桥接的 plugin.* 命名空间转发到 WASM，
 * 再把结果 postMessage 回 iframe。天工不关心消息内容，只做透传。
 */
export function PluginIframe({ pluginId, html }: { pluginId: string; html: string }) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const theme = useResolvedTheme();
  const channel = useMemo(() => crypto.randomUUID(), [pluginId, html]);
  const maskColor = usePluginMask(iframeRef, channel);

  const sendHostContext = useCallback(() => {
    iframeRef.current?.contentWindow?.postMessage(hostContext(theme, channel), '*');
  }, [channel, theme]);

  useEffect(() => {
    sendHostContext();
  }, [sendHostContext]);

  useEffect(() => {
    const source = iframeRef.current?.contentWindow;
    if (!source) return;
    const handler = (event: MessageEvent) => {
      if (event.source !== source) return;
      const data = event.data;
      if (!data || data.type !== 'plugin_call' || data.channel !== channel) return;
      const { id, method, payload } = data;
      if (
        typeof id !== 'string'
        || typeof method !== 'string'
        || typeof payload !== 'string'
        || id.length > 200
        || method.length > 100
        || payload.length > 2_000_000
      ) return;

      const queue = pluginCallQueues.get(pluginId) ?? Promise.resolve();
      pluginCallQueues.set(pluginId, queue.then(async () => {
        try {
          // 经宿主桥接的 plugin.* 命名空间转发（命名空间校验 + WASM 透传）
          const result = await api.bridgeCall(pluginId, `plugin.${method}`, payload);
          source.postMessage({ id, channel, result }, '*');
        } catch (e) {
          source.postMessage(
            { id, channel, error: String(e) },
            '*',
          );
        }
      }));
    };

    window.addEventListener('message', handler);
    return () => {
      window.setTimeout(() => window.removeEventListener('message', handler), 1000);
    };
  }, [channel, pluginId]);

  return (
    <div className="flex h-full min-h-0 min-w-0 w-full flex-1 flex-col overflow-hidden">
      {maskColor && (
        <div
          aria-hidden="true"
          data-plugin-host-mask
          className="fixed inset-0 z-[90]"
          style={{ backgroundColor: maskColor }}
        />
      )}
      <iframe
        ref={iframeRef}
        title="plugin-view"
        srcDoc={html}
        className={`block min-h-0 min-w-0 w-full flex-1 border-0 ${maskColor ? 'relative z-[91]' : ''}`}
        sandbox="allow-scripts"
        onLoad={sendHostContext}
      />
    </div>
  );
}
