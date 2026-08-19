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
  browser_open: 'webview.create',
  browser_navigate: 'webview.navigate',
  browser_eval: 'webview.eval',
  // 协作工具（策略在 TS，引擎经协作原语）：
  web_fetch: 'webview.fetch',
  web_query_dom: 'webview.queryDom',
  web_click: 'webview.click',
  web_form_fill: 'webview.formFill',
  web_form_extract: 'webview.formExtract',
  web_locate_element: 'webview.locate',
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
        // 会话绑定（对齐终端插件）：Agent 打开/操作的页面归属发起对话，
        // 与该对话的浏览器面板是同一实例（插件×会话双维度隔离）。
        const raw = await bridge.call(
          method,
          JSON.stringify({
            ...((invocation.arguments as Record<string, unknown>) ?? {}),
            session_id: invocation.session_id,
          }),
        );
        const parsed = JSON.parse(raw) as {
          view_id?: string;
          tabs?: Array<{ id?: string; url?: string; title?: string }>;
          active_tab_id?: string | null;
          result?: string | null;
        };
        // 真实结果摘要：按工具类别格式化（策略层职责）
        let summary: string;
        if (invocation.name === 'browser_eval') {
          summary = parsed.result ?? '(无返回值)';
        } else if (invocation.name === 'web_fetch') {
          const content = (parsed as { content?: string }).content ?? '';
          summary = content.slice(0, 2_000_000) || '(空内容)';
        } else if (
          invocation.name === 'web_query_dom' ||
          invocation.name === 'web_form_extract' ||
          invocation.name === 'web_locate_element'
        ) {
          summary = JSON.stringify(parsed).slice(0, 100_000);
        } else if (
          invocation.name === 'web_click' ||
          invocation.name === 'web_form_fill'
        ) {
          summary = JSON.stringify(parsed).slice(0, 10_000);
        } else {
          const tabs = parsed.tabs ?? [];
          const active = tabs.find((tab) => tab.id === parsed.active_tab_id) ?? tabs[0];
          summary = active
            ? `webview 实例 ${parsed.view_id ?? '?'}，当前页：${active.title ?? active.url ?? '未知'}`
            : `webview 实例 ${parsed.view_id ?? '?'} 已就绪`;
        }
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: { ok: true, summary, exit_code: 0 },
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
