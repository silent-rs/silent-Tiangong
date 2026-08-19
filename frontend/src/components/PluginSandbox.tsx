import { useEffect, useRef } from 'react';
import { api, type SandboxKind } from '../api/tauri';
import { PluginIframe } from './PluginIframe';
import {
  hostContext,
  type PluginHostContext,
} from './pluginHostContext';

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
  sessionId?: string | null;
  /** 当前会话工作目录（无活跃会话时为全局工作区）。 */
  workspace?: string | null;
}

export function PluginSandbox({
  sandbox,
  pluginId,
  contributionId,
  html,
  className,
  sessionId,
  workspace,
}: PluginSandboxProps) {
  if (sandbox === 'shadow') {
    return (
      <ShadowContainer
        pluginId={pluginId}
        contributionId={contributionId}
        html={html}
        className={className}
        sessionId={sessionId}
        workspace={workspace}
      />
    );
  }
  if (sandbox === 'native') {
    return (
      <div className="p-4 text-sm text-muted-foreground">
        该贡献声明 native 容器，仅官方签名插件可用，当前版本暂未提供。
      </div>
    );
  }
  return <PluginIframe pluginId={pluginId} html={html} sessionId={sessionId} workspace={workspace} />;
}

/** 宿主注入插件脚本的桥接对象（设计文档 6.3 的 Shadow 容器子集）。 */
export interface HostBridge {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}

interface DisposableHostBridge extends HostBridge {
  dispose(): void;
}

type ShadowCleanup = () => void;
type HostContextHandler = (context: PluginHostContext) => void;

interface ShadowRuntimeState {
  updateContext(context: PluginHostContext): void;
}

function currentRootTheme(): 'light' | 'dark' {
  return document.documentElement.classList.contains('dark') ? 'dark' : 'light';
}

/**
 * 创建绑定单个插件贡献的桥接：call 走 T003 白名单通道；
 * on 经 bridge_subscribe 订阅 + 全局 bridge_event 分发，dispose 时全部退订。
 */
function createHostBridge(pluginId: string): DisposableHostBridge {
  let disposed = false;
  const channelHandlers = new Map<string, Set<(payload: string) => void>>();
  const subscribedChannels = new Set<string>();
  const pendingSubscriptions = new Set<Promise<void>>();
  let unlistenEvent: (() => void) | null = null;
  let eventListeningTask: Promise<void> | null = null;

  const ensureEventListening = () => {
    if (unlistenEvent || disposed) return Promise.resolve();
    if (eventListeningTask) return eventListeningTask;
    eventListeningTask = api.onBridgeEvent((event) => {
      if (event.plugin_id !== pluginId) return;
      channelHandlers.get(event.channel)?.forEach((handler) => handler(event.payload));
    }).then((stop) => {
      if (disposed) {
        stop();
      } else {
        unlistenEvent = stop;
      }
    }).finally(() => {
      eventListeningTask = null;
    });
    return eventListeningTask;
  };

  return {
    async call(method, payload) {
      if (disposed) throw new Error('bridge 已随容器卸载');
      // 插件常在 bridge.on 后立即启动会产生通知的 sidecar。先等宿主订阅
      // 真正生效，避免首批输出在 invoke 与 subscribe 的竞态中丢失。
      await Promise.all([...pendingSubscriptions]);
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
        const subscription = Promise.all([
          api.bridgeSubscribe(pluginId, channel),
          ensureEventListening(),
        ])
          .catch((error) => {
            console.warn(`[plugin-sandbox] 订阅 ${channel} 失败:`, error);
          })
          .then(() => undefined)
          .finally(() => pendingSubscriptions.delete(subscription));
        pendingSubscriptions.add(subscription);
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
      pendingSubscriptions.clear();
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
function ShadowContainer({
  pluginId,
  contributionId,
  html,
  className,
  sessionId,
  workspace,
}: Omit<PluginSandboxProps, 'sandbox'>) {
  const hostRef = useRef<HTMLDivElement>(null);
  const runtimeRef = useRef<ShadowRuntimeState | null>(null);

  // 挂载/重建：解析 HTML → 取回外链资源 → 注入 shadow root → 按序执行脚本。
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const shadow = host.shadowRoot ?? host.attachShadow({ mode: 'open' });
    const bridge = createHostBridge(pluginId);
    let cancelled = false;
    let currentContext = hostContext(
      currentRootTheme(),
      `shadow:${pluginId}:${contributionId}`,
      sessionId,
      workspace,
    );
    const contextHandlers = new Set<HostContextHandler>();
    const cleanups: ShadowCleanup[] = [];

    const runCleanup = (cleanup: ShadowCleanup) => {
      try {
        cleanup();
      } catch (error) {
        console.warn('[plugin-sandbox] 插件卸载回调执行失败:', error);
      }
    };

    const registerCleanup = (cleanup: ShadowCleanup) => {
      if (cancelled) {
        runCleanup(cleanup);
        return;
      }
      cleanups.push(cleanup);
    };

    const onHostContextChange = (handler: HostContextHandler) => {
      if (cancelled) return () => {};
      contextHandlers.add(handler);
      handler(currentContext);
      return () => contextHandlers.delete(handler);
    };

    const runtime: ShadowRuntimeState = {
      updateContext(context) {
        const contextChanged = currentContext.session?.id !== context.session?.id
          || currentContext.session?.workspace !== context.session?.workspace;
        currentContext = context;
        if (!contextChanged) return;
        contextHandlers.forEach((handler) => handler(context));
      },
    };
    runtimeRef.current = runtime;

    void mountShadowContent(
      shadow,
      html,
      pluginId,
      contributionId,
      bridge,
      () => currentContext,
      onHostContextChange,
      registerCleanup,
      () => cancelled,
    );

    return () => {
      cancelled = true;
      if (runtimeRef.current === runtime) runtimeRef.current = null;
      for (const cleanup of cleanups.reverse()) runCleanup(cleanup);
      contextHandlers.clear();
      bridge.dispose();
      shadow.innerHTML = '';
    };
    // 主题变量从 App 根节点自动继承，主题和会话变化都不重建插件页面。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pluginId, contributionId, html]);

  // 会话切换时刷新运行时上下文；主题样式由 CSS 继承自动更新。
  useEffect(() => {
    runtimeRef.current?.updateContext(
      hostContext(currentRootTheme(), `shadow:${pluginId}:${contributionId}`, sessionId, workspace),
    );
  }, [contributionId, pluginId, sessionId, workspace]);

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
 * 3. 脚本（内联 + 取回的外链）按文档顺序拼接后受控执行。旧脚本仍可只使用
 *    `bridge`；新脚本还可使用 `pluginRoot`、宿主上下文订阅与卸载登记。
 *
 * 图片等媒体资源暂不代理（后续按需扩展），当前面向脚本与样式驱动的页面。
 */
async function mountShadowContent(
  shadow: ShadowRoot,
  html: string,
  pluginId: string,
  contributionId: string,
  bridge: HostBridge,
  getHostContext: () => PluginHostContext,
  onHostContextChange: (handler: HostContextHandler) => () => void,
  registerCleanup: (cleanup: ShadowCleanup) => void,
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
    const runner = new Function(
      'bridge',
      'pluginRoot',
      'hostContext',
      'onHostContextChange',
      'registerCleanup',
      `"use strict";\n${combined}`,
    );
    const returnedCleanup = runner.call(
      shadow,
      bridge,
      shadow,
      getHostContext(),
      onHostContextChange,
      registerCleanup,
    );
    if (typeof returnedCleanup === 'function') registerCleanup(returnedCleanup as ShadowCleanup);
  } catch (error) {
    console.error('[plugin-sandbox] 插件脚本执行失败:', error);
  }
}
