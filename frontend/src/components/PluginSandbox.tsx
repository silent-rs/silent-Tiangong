import { useEffect, useRef } from 'react';
import { api, type SandboxKind } from '../api/tauri';
import { useResolvedTheme } from '../hooks/useTheme';
import { PluginIframe } from './PluginIframe';
import { applyShadowThemeTokens } from './pluginHostContext';

/**
 * 标准 Slot 沙箱容器：按贡献声明的 sandbox 级别分发渲染。
 *
 * - shadow（默认）：挂载主 DOM 树的 Shadow DOM 容器，插件脚本以注入的
 *   `bridge` 参数访问宿主桥接（设计文档 6.2 ①）。
 * - iframe：等价既有 srcdoc + postMessage 模式的强隔离容器（6.2 ②）。
 * - native：官方原生容器（6.2 ③），M2 内置插件迁移时提供。
 *
 * 宿主只负责容器与桥接通道，不解析插件页面内容。
 */
export interface PluginSandboxProps {
  pluginId: string;
  contributionId: string;
  sandbox: SandboxKind;
  html: string;
  className?: string;
}

export function PluginSandbox({ sandbox, pluginId, contributionId, html, className }: PluginSandboxProps) {
  if (sandbox === 'shadow') {
    return <ShadowContainer pluginId={pluginId} contributionId={contributionId} html={html} className={className} />;
  }
  if (sandbox === 'native') {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        该贡献声明 native 容器，仅官方签名插件可用，当前版本暂未提供。
      </div>
    );
  }
  return <PluginIframe pluginId={pluginId} html={html} />;
}

/** 宿主注入插件脚本的桥接对象（设计文档 6.3 的 Shadow 容器子集）。 */
export interface HostBridge {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}

interface DisposableHostBridge extends HostBridge {
  dispose(): void;
}

/**
 * 创建绑定单个插件贡献的桥接：call 走 T003 白名单通道；
 * on 经 bridge_subscribe 订阅 + 全局 bridge_event 分发，dispose 时全部退订。
 */
function createHostBridge(pluginId: string): DisposableHostBridge {
  let disposed = false;
  const channelHandlers = new Map<string, Set<(payload: string) => void>>();
  const subscribedChannels = new Set<string>();
  let unlistenEvent: (() => void) | null = null;

  const ensureEventListening = async () => {
    if (unlistenEvent || disposed) return;
    const stop = await api.onBridgeEvent((event) => {
      if (event.plugin_id !== pluginId) return;
      channelHandlers.get(event.channel)?.forEach((handler) => handler(event.payload));
    });
    if (disposed) {
      stop();
    } else {
      unlistenEvent = stop;
    }
  };

  return {
    async call(method, payload) {
      if (disposed) throw new Error('bridge 已随容器卸载');
      return api.bridgeCall(pluginId, method, payload);
    },
    on(channel, handler) {
      let handlers = channelHandlers.get(channel);
      if (!handlers) {
        handlers = new Set();
        channelHandlers.set(channel, handlers);
      }
      handlers.add(handler);
      if (!subscribedChannels.has(channel)) {
        subscribedChannels.add(channel);
        api.bridgeSubscribe(pluginId, channel).catch((error) => {
          console.warn(`[plugin-sandbox] 订阅 ${channel} 失败:`, error);
        });
        void ensureEventListening();
      }
      return () => {
        handlers?.delete(handler);
        if (handlers?.size === 0) {
          channelHandlers.delete(channel);
          subscribedChannels.delete(channel);
          api.bridgeUnsubscribe(pluginId, channel).catch(() => {});
        }
      };
    },
    dispose() {
      disposed = true;
      channelHandlers.clear();
      for (const channel of subscribedChannels) {
        api.bridgeUnsubscribe(pluginId, channel).catch(() => {});
      }
      subscribedChannels.clear();
      unlistenEvent?.();
      unlistenEvent = null;
    },
  };
}

/** Shadow DOM 沙箱容器：入口 HTML 注入 shadow root，脚本受控执行。 */
function ShadowContainer({ pluginId, contributionId, html, className }: Omit<PluginSandboxProps, 'sandbox'>) {
  const hostRef = useRef<HTMLDivElement>(null);
  const theme = useResolvedTheme();

  // 挂载/重建：解析 HTML → 取回外链资源 → 注入 shadow root → 按序执行脚本。
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const shadow = host.shadowRoot ?? host.attachShadow({ mode: 'open' });
    const bridge = createHostBridge(pluginId);
    let cancelled = false;

    applyShadowThemeTokens(shadow, theme);
    void mountShadowContent(shadow, html, pluginId, contributionId, bridge, () => cancelled);

    return () => {
      cancelled = true;
      bridge.dispose();
      shadow.innerHTML = '';
    };
    // theme 不参与重建：token 由独立 effect 刷新，避免主题切换重载插件页面。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pluginId, contributionId, html]);

  // 主题切换：仅刷新 :host CSS 变量，不重建内容。
  useEffect(() => {
    const shadow = hostRef.current?.shadowRoot;
    if (shadow) applyShadowThemeTokens(shadow, theme);
  }, [theme]);

  return (
    <div
      ref={hostRef}
      data-plugin-shadow-host={`${pluginId}:${contributionId}`}
      className={className ?? 'block h-full min-h-0 min-w-0 w-full flex-1 overflow-auto'}
    />
  );
}

/**
 * 把入口 HTML 渲染进 shadow root：
 * 1. DOMParser 解析；`<link rel=stylesheet>` 与 `<script src>` 经宿主按
 *    插件目录安全读取（外链资源不落任意 URL，见 read_manifest_resource）；
 * 2. 其余节点（含内联 style）按序导入 shadow root；
 * 3. 脚本（内联 + 取回的外链）按文档顺序拼接后经 `new Function('bridge', …)`
 *    受控执行：插件脚本以参数拿到桥接对象，不污染全局命名空间。
 *
 * 图片等媒体资源暂不代理（后续按需扩展），当前面向脚本与样式驱动的页面。
 */
async function mountShadowContent(
  shadow: ShadowRoot,
  html: string,
  pluginId: string,
  contributionId: string,
  bridge: HostBridge,
  isCancelled: () => boolean,
) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const nodes = [...doc.head.childNodes, ...doc.body.childNodes];
  const scripts: string[] = [];

  const fetchText = async (path: string): Promise<string> => {
    const resource = await api.pluginReadEntryResource(pluginId, contributionId, path);
    // 字节数组 → 文本（脚本/样式按 UTF-8 解码）
    return new TextDecoder().decode(new Uint8Array(resource.data));
  };

  for (const node of nodes) {
    if (isCancelled()) return;
    if (!(node instanceof Element)) {
      if (node.textContent?.trim()) shadow.appendChild(document.importNode(node, true));
      continue;
    }
    if (node.tagName === 'SCRIPT') {
      const src = node.getAttribute('src');
      if (src) {
        try {
          const code = await fetchText(src);
          scripts.push(code);
        } catch (error) {
          console.warn(`[plugin-sandbox] 加载脚本 ${src} 失败:`, error);
        }
      } else if (node.textContent) {
        scripts.push(node.textContent);
      }
      continue;
    }
    if (node.tagName === 'LINK' && node.getAttribute('rel')?.toLowerCase() === 'stylesheet') {
      const href = node.getAttribute('href');
      if (href) {
        try {
          const css = await fetchText(href);
          const style = document.createElement('style');
          style.setAttribute('data-source-href', href);
          style.textContent = css;
          shadow.appendChild(style);
        } catch (error) {
          console.warn(`[plugin-sandbox] 加载样式 ${href} 失败:`, error);
        }
      }
      continue;
    }
    shadow.appendChild(document.importNode(node, true));
  }

  if (isCancelled() || scripts.length === 0) return;
  try {
    const combined = scripts.join('\n;\n');
    const runner = new Function('bridge', `"use strict";\n${combined}`);
    runner.call(shadow, bridge);
  } catch (error) {
    console.error('[plugin-sandbox] 插件脚本执行失败:', error);
  }
}
