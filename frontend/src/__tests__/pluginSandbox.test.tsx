import { act, type ReactNode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// 沙箱容器依赖的宿主 API 与 hooks 按最小面 mock。
const mocks = vi.hoisted(() => {
  const bridgeCall = vi.fn(() => Promise.resolve('{"ok":true}'));
  const bridgeSubscribe = vi.fn(() => Promise.resolve());
  const bridgeUnsubscribe = vi.fn(() => Promise.resolve());
  const onBridgeEventHandlers = new Set<(event: { payload: unknown }) => void>();
  const onBridgeEvent = vi.fn((callback: (event: unknown) => void) => {
    const wrapped = (event: { payload: unknown }) => callback(event.payload);
    onBridgeEventHandlers.add(wrapped);
    return Promise.resolve(() => onBridgeEventHandlers.delete(wrapped));
  });
  const resources = new Map<string, { data: number[]; mime: string }>();
  const pluginReadEntryResource = vi.fn(
    (pluginId: string, contributionId: string, path: string) => {
      const resource = resources.get(path);
      if (!resource) {
        return Promise.reject(new Error(`无资源 ${path}`));
      }
      return Promise.resolve(resource);
    },
  );
  return {
    bridgeCall,
    bridgeSubscribe,
    bridgeUnsubscribe,
    onBridgeEvent,
    onBridgeEventHandlers,
    resources,
    pluginReadEntryResource,
  };
});

vi.mock('@/api/tauri', () => ({
  api: {
    bridgeCall: mocks.bridgeCall,
    bridgeSubscribe: mocks.bridgeSubscribe,
    bridgeUnsubscribe: mocks.bridgeUnsubscribe,
    onBridgeEvent: mocks.onBridgeEvent,
    pluginReadEntryResource: mocks.pluginReadEntryResource,
  },
}));

vi.mock('@/hooks/useTheme', () => ({
  useResolvedTheme: () => 'dark',
}));

vi.mock('@/hooks/usePluginMask', () => ({
  usePluginMask: () => undefined,
}));

import { PluginSandbox } from '@/components/PluginSandbox';

let container: HTMLDivElement | null = null;
let root: Root | null = null;

const render = async (node: ReactNode) => {
  container = document.createElement('div');
  document.body.appendChild(container);
  root = createRoot(container);
  root.render(node);
  // mountShadowContent 为 async（外链资源取回 + 脚本执行），刷 microtask 等其完成。
  await act(async () => {});
  await act(async () => {});
};

afterEach(() => {
  if (root) {
    act(() => root!.unmount());
  }
  container?.remove();
  container = null;
  root = null;
  vi.clearAllMocks();
  mocks.resources.clear();
  mocks.onBridgeEventHandlers.clear();
  delete (window as unknown as { __received?: string[] }).__received;
  delete (window as unknown as { __shadowBoots?: number }).__shadowBoots;
  delete (window as unknown as { __shadowContexts?: string[] }).__shadowContexts;
  delete (window as unknown as { __shadowCleanup?: number }).__shadowCleanup;
  delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
});

/** 脚本内以 bridge 参数回传调用记录（Shadow 容器执行插件的出口）。 */
const BRIDGE_CALL_LOG: Array<{ method: string; payload: string }> = [];

beforeEach(() => {
  BRIDGE_CALL_LOG.length = 0;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

const shadowHost = () => container?.querySelector<HTMLDivElement>('[data-plugin-shadow-host]');

describe('PluginSandbox 沙箱容器', () => {
  it('shadow 模式把入口 HTML 注入 shadow root 并以 bridge 参数执行内联脚本', async () => {
    const html = `
      <head><style>.card { color: var(--foreground); }</style></head>
      <body>
        <div class="card">看板</div>
        <script>bridge.call('plugin.ping', '{}');</script>
      </body>`;
    await render(
      <PluginSandbox pluginId="com.example.board" contributionId="app-main" sandbox="shadow" html={html} />,
    );

    const host = shadowHost();
    expect(host, '应渲染 shadow 宿主元素').toBeTruthy();
    const shadow = host!.shadowRoot!;
    expect(shadow.querySelector('.card')?.textContent).toBe('看板');
    const cardStyle = [...shadow.querySelectorAll('style')]
      .find((style) => style.textContent?.includes('.card'));
    expect(cardStyle, '插件内联样式应注入 shadow root').toBeTruthy();
    // 脚本以注入的 bridge 执行：调用经 api.bridgeCall 转发
    expect(mocks.bridgeCall).toHaveBeenCalledWith('com.example.board', 'plugin.ping', '{}');
  });

  it('shadow 模式注入宿主主题 token 为 :host CSS 变量', async () => {
    await render(
      <PluginSandbox pluginId="p" contributionId="c" sandbox="shadow" html="<body><div>x</div></body>" />,
    );
    const style = shadowHost()!.shadowRoot!.querySelector<HTMLStyleElement>('style[data-host-tokens]');
    expect(style).toBeTruthy();
    expect(style!.textContent).toContain(':host');
    expect(style!.textContent).toContain('--host-theme: dark');
  });

  it('shadow 模式注入插件根节点并在会话变化时推送上下文且不重建脚本', async () => {
    const html = `<body><div id="session"></div><script>
      window.__shadowBoots = (window.__shadowBoots || 0) + 1;
      const sessionNode = pluginRoot.querySelector('#session');
      sessionNode.textContent = hostContext.session?.id || '';
      window.__shadowContexts = [];
      onHostContextChange((context) => {
        const sessionId = context.session?.id || '';
        sessionNode.textContent = sessionId;
        window.__shadowContexts.push(sessionId);
      });
    </script></body>`;
    await render(
      <PluginSandbox
        pluginId="context-plugin"
        contributionId="main"
        sandbox="shadow"
        html={html}
        sessionId="session-a"
      />,
    );

    expect(shadowHost()!.shadowRoot!.querySelector('#session')?.textContent).toBe('session-a');
    expect((window as unknown as { __shadowBoots: number }).__shadowBoots).toBe(1);

    await act(async () => {
      root!.render(
        <PluginSandbox
          pluginId="context-plugin"
          contributionId="main"
          sandbox="shadow"
          html={html}
          sessionId="session-b"
        />,
      );
    });

    expect(shadowHost()!.shadowRoot!.querySelector('#session')?.textContent).toBe('session-b');
    expect((window as unknown as { __shadowBoots: number }).__shadowBoots).toBe(1);
    expect((window as unknown as { __shadowContexts: string[] }).__shadowContexts)
      .toEqual(['session-a', 'session-b']);
  });

  it('外链脚本与样式经宿主按插件目录读取后注入执行', async () => {
    mocks.resources.set('app.js', {
      data: Array.from(new TextEncoder().encode("bridge.call('plugin.boot', '{\"v\":1}')")),
      mime: 'text/javascript',
    });
    mocks.resources.set('ui.css', {
      data: Array.from(new TextEncoder().encode('.root { padding: 4px; }')),
      mime: 'text/css',
    });
    const html = `
      <head><link rel="stylesheet" href="ui.css"></head>
      <body><div id="root"></div><script src="app.js"></script></body>`;
    await render(
      <PluginSandbox pluginId="p2" contributionId="c2" sandbox="shadow" html={html} />,
    );

    expect(mocks.pluginReadEntryResource).toHaveBeenCalledWith('p2', 'c2', 'app.js');
    expect(mocks.pluginReadEntryResource).toHaveBeenCalledWith('p2', 'c2', 'ui.css');
    const shadow = shadowHost()!.shadowRoot!;
    expect(shadow.querySelector('style[data-source-href="ui.css"]')?.textContent).toContain('.root');
    expect(mocks.bridgeCall).toHaveBeenCalledWith('p2', 'plugin.boot', '{"v":1}');
  });

  it('外链脚本加载失败不阻断其余内容渲染', async () => {
    const html = `
      <body><div id="safe">内容</div><script src="missing.js"></script>
      <script>bridge.call('plugin.after', '{}');</script></body>`;
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    await render(<PluginSandbox pluginId="p3" contributionId="c3" sandbox="shadow" html={html} />);

    expect(shadowHost()!.shadowRoot!.querySelector('#safe')?.textContent).toBe('内容');
    expect(mocks.bridgeCall).toHaveBeenCalledWith('p3', 'plugin.after', '{}');
    warn.mockRestore();
  });

  it('卸载时清理 shadow 内容并退订事件通道', async () => {
    const subscribeHtml = `<body><div>y</div><script>
      bridge.on('session.updated', () => {});
      bridge.call('plugin.sub', '{}');
      registerCleanup(() => { window.__shadowCleanup = (window.__shadowCleanup || 0) + 1; });
    </script></body>`;
    await render(
      <PluginSandbox pluginId="p4" contributionId="c4" sandbox="shadow" html={subscribeHtml} />,
    );
    expect(mocks.bridgeSubscribe).toHaveBeenCalledWith('p4', 'session.updated');

    act(() => root!.unmount());
    expect(mocks.bridgeUnsubscribe).toHaveBeenCalledWith('p4', 'session.updated');
    expect((window as unknown as { __shadowCleanup: number }).__shadowCleanup).toBe(1);
    expect(shadowHost()).toBeNull();
    root = null;
  });

  it('bridge.on 的事件按 plugin_id 分发给对应容器', async () => {
    const received: string[] = [];
    const html = `<body><div>z</div><script>
      bridge.on('tool.started', (payload) => { window.__received = window.__received || []; window.__received.push(payload); });
    </script></body>`;
    await render(<PluginSandbox pluginId="p5" contributionId="c5" sandbox="shadow" html={html} />);
    await act(async () => {});

    // 模拟宿主推送：仅 p5 的事件到达
    for (const handler of mocks.onBridgeEventHandlers) {
      handler({ payload: { plugin_id: 'other', channel: 'tool.started', payload: 'x' } });
      handler({ payload: { plugin_id: 'p5', channel: 'tool.started', payload: 'hit' } });
    }
    expect((window as unknown as { __received: string[] }).__received).toEqual(['hit']);
  });

  it('iframe 模式复用 PluginIframe 通道（渲染 iframe 元素）', async () => {
    await render(
      <PluginSandbox pluginId="p6" contributionId="c6" sandbox="iframe" html="<body>b</body>" />,
    );
    expect(container!.querySelector('iframe')).toBeTruthy();
    expect(shadowHost()).toBeNull();
  });

  it('native 模式给出占位说明', async () => {
    await render(
      <PluginSandbox pluginId="p7" contributionId="c7" sandbox="native" html="" />,
    );
    expect(container!.textContent).toContain('native 容器');
  });
});
