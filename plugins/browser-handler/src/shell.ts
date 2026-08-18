import {
  createTiangongBridge,
  createToolProvider,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';

/**
 * 浏览器插件 TS 壳（阶段 4 完全体雏形）：
 * - 工具执行路由到宿主 webview 容器原语（bridge webview.*）；
 * - webview 实例创建/导航/生命周期策略在本插件；
 * - 管理界面（地址栏/工具栏，shadow DOM）与容器声明见 App.vue。
 */

const TOOL_METHOD: Record<string, string> = {
  browser_open: 'webview.navigate',
  browser_navigate: 'webview.navigate',
  browser_eval: 'webview.eval',
};

async function main() {
  const bridge = await createTiangongBridge();
  const tools = createToolProvider(bridge);

  tools.onRequested((invocation: ToolInvocation) => {
    void (async () => {
      const method = TOOL_METHOD[invocation.name];
      if (!method) {
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'cancelled',
          result: { ok: false, summary: `未知工具 ${invocation.name}`, exit_code: 1 },
        });
        return;
      }
      try {
        const raw = await bridge.call(method, JSON.stringify(invocation.arguments ?? {}));
        const parsed = JSON.parse(raw) as { supported?: boolean; note?: string };
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: {
            ok: parsed.supported !== false,
            summary: parsed.note ?? '操作已提交 webview 容器',
            exit_code: 0,
          },
        });
      } catch (error) {
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: { ok: false, summary: `webview 调用失败：${String(error)}`, exit_code: 1 },
        });
      }
    })();
  });
}

void main();
