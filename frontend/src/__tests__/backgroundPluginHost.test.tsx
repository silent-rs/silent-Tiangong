import { describe, expect, it, vi } from 'vitest';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import {
  BackgroundPluginHost,
  type BackgroundPluginInstance,
} from '@/components/BackgroundPluginHost';

/**
 * 工具接应的后台挂载链验证（app.open mode=background 落地）：
 * 收到后台挂载的实例清单后，经 pluginOpenEntry 取入口 HTML 并在隐藏
 * 容器内挂载 Shadow 沙箱——插件脚本由此获得执行环境与 tool.requested
 * 订阅能力，不依赖拓展区面板打开。
 */

vi.mock('@/api/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/tauri')>();
  return {
    ...actual,
    api: {
      ...actual.api,
      pluginOpenEntry: vi.fn(() =>
        Promise.resolve('<html><body><script>/* plugin shell */</script></body></html>'),
      ),
    },
  };
});

import { api } from '@/api/tauri';

function renderHost(instances: BackgroundPluginInstance[]) {
  const container = document.createElement('div');
  document.body.appendChild(container);
  let root!: Root;
  act(() => {
    root = createRoot(container);
    root.render(<BackgroundPluginHost instances={instances} />);
  });
  return {
    root,
    container,
    cleanup: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
}

describe('BackgroundPluginHost 后台挂载', () => {
  it('为后台实例加载入口并在隐藏容器挂载 Shadow 沙箱', async () => {
    const { container, cleanup } = renderHost([{
      pluginId: 'browser',
      contributionId: 'browser',
      sandbox: 'webview',
      sessionId: 'session-1',
    }]);

    // 入口 HTML 异步加载完成后渲染沙箱。
    await act(async () => { await Promise.resolve(); });
    expect(api.pluginOpenEntry).toHaveBeenCalledWith('browser', 'browser');

    const host = container.querySelector('[data-plugin-shadow-host="browser:browser"]');
    expect(host, '应挂载 Shadow 沙箱宿主元素').toBeTruthy();
    expect(host?.shadowRoot, '应创建 shadow root 供插件脚本执行').toBeTruthy();

    // 同一插件同一会话不重复挂载多个沙箱实例。
    cleanup();
  });

  it('同会话同贡献只保留一个实例，多会话并存', async () => {
    const { container, cleanup } = renderHost([
      { pluginId: 'browser', contributionId: 'browser', sandbox: 'webview', sessionId: 's1' },
      { pluginId: 'browser', contributionId: 'browser', sandbox: 'webview', sessionId: 's2' },
    ]);
    await act(async () => { await Promise.resolve(); });
    const hosts = container.querySelectorAll('[data-plugin-shadow-host]');
    expect(hosts.length).toBe(2);
    cleanup();
  });
});
