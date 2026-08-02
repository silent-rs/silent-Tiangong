import { useEffect, useState, useRef, useCallback, useMemo } from 'react';
import { api, type PluginContributionEntry } from '../api/tauri';
import { useResolvedTheme } from '../hooks/useTheme';
import { usePluginMask } from '../hooks/usePluginMask';

/**
 * 插件设置面板：通用 iframe 容器。
 *
 * 天工不处理插件页面内容，只提供容器和桥接：
 * - 列出插件 contributions（声明是否有页面）
 * - 用户点击进入时，调 pluginOpenView 获取 HTML → 渲染到 iframe（srcdoc）
 * - iframe 内 JS 经 postMessage → 本组件 → pluginCall → WASM handle-view-message
 *
 * 所有业务逻辑在 WASM 插件内部处理，天工完全通用。
 */
export function PluginSettingsPanel() {
  const [contributions, setContributions] = useState<PluginContributionEntry[]>([]);
  const [selected, setSelected] = useState<PluginContributionEntry | null>(null);
  const [html, setHtml] = useState<string>('');
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    api.listPluginContributions()
      .then((entries) => {
        setContributions(entries);
      })
      .catch(() => setContributions([]))
      .finally(() => setLoading(false));
  }, []);

  // 用户选中某个插件时，按需获取页面 HTML。
  const selectContribution = useCallback(async (entry: PluginContributionEntry) => {
    setSelected(entry);
    if (!entry.has_view) {
      setHtml('');
      return;
    }
    try {
      const pageHtml = await api.pluginOpenView(entry.plugin_id, entry.contribution_id);
      setHtml(pageHtml);
    } catch {
      setHtml('<p style="padding:16px;color:#888">页面加载失败</p>');
    }
  }, []);

  if (loading) {
    return <div className="p-4 text-sm text-muted-foreground">加载插件列表…</div>;
  }

  if (contributions.length === 0) {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        暂无已加载的 WASM 插件。
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 min-w-0 overflow-hidden">
      {/* 左侧：插件列表 */}
      <div className="w-48 border-r pr-2 space-y-1">
        {contributions.map((entry) => (
          <button
            key={`${entry.plugin_id}:${entry.contribution_id}`}
            onClick={() => selectContribution(entry)}
            className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
              selected?.plugin_id === entry.plugin_id
                ? 'bg-accent text-accent-foreground'
                : 'hover:bg-accent/50'
            }`}
          >
            {entry.title}
          </button>
        ))}
      </div>

      {/* 右侧：插件页面（iframe 容器） */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
        {selected && html ? (
          <PluginIframe
            pluginId={selected.plugin_id}
            html={html}
          />
        ) : selected ? (
          <div className="p-4 text-sm text-muted-foreground">该插件无可配置页面。</div>
        ) : (
          <div className="p-4 text-sm text-muted-foreground">请从左侧选择一个插件。</div>
        )}
      </div>
    </div>
  );
}

const HOST_TOKEN_NAMES = [
  'background',
  'foreground',
  'card',
  'card-foreground',
  'muted',
  'muted-foreground',
  'accent',
  'accent-foreground',
  'primary',
  'primary-foreground',
  'destructive',
  'border',
  'input',
  'ring',
  'status-success',
  'status-warning',
  'status-error',
  'status-info',
  'radius',
] as const;

function hostContext(theme: 'light' | 'dark', channel: string) {
  const styles = getComputedStyle(document.documentElement);
  return {
    type: 'tiangong_host_context',
    channel,
    theme,
    tokens: Object.fromEntries(
      HOST_TOKEN_NAMES.map((name) => [name, styles.getPropertyValue(`--${name}`).trim()]),
    ),
    fontFamily: getComputedStyle(document.body).fontFamily,
  };
}

const pluginCallQueues = new Map<string, Promise<void>>();

/**
 * 插件 iframe 容器 + postMessage 桥接。
 *
 * iframe 内的 JS 经 window.parent.postMessage({ type: 'plugin_call', ... }) 发消息，
 * 本组件收到后调 api.pluginCall 转发到 WASM，再把结果 postMessage 回 iframe。
 * 天工不关心消息内容，只做透传。
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
          const result = await api.pluginCall(pluginId, method, payload);
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
        title="plugin-settings"
        srcDoc={html}
        className={`block min-h-0 min-w-0 w-full flex-1 border-0 ${maskColor ? 'relative z-[91]' : ''}`}
        sandbox="allow-scripts"
        onLoad={sendHostContext}
      />
    </div>
  );
}
