import {
  createTiangongBridge,
  createToolProvider,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';
import { tabsModel } from './tabs-model';

/**
 * 浏览器插件 TS 壳：
 * - 打开/导航经共享标签模型（tabs-model，与面板同源）；
 * - 求值与页面协作路由到宿主 webview 容器原语（bridge webview.*）；
 * - 管理界面（地址栏/工具栏，shadow DOM）见 App.vue。
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
        // 打开/导航经共享标签模型（与面板同源，阶段 3 标签语义在插件）；
        // 发起会话与面板当前会话一致时走模型，否则退回原语直调（后台会话）。
        if (
          (invocation.name === 'browser_open' || invocation.name === 'browser_navigate') &&
          tabsModel.scope === invocation.session_id &&
          typeof (invocation.arguments as { url?: unknown })?.url === 'string'
        ) {
          const target = (invocation.arguments as { url: string }).url;
          if (invocation.name === 'browser_open' || tabsModel.tabs.length === 0) {
            await tabsModel.newTab(target);
          } else {
            await tabsModel.navigate(target);
          }
          const summary =
            invocation.name === 'browser_open'
              ? `已在浏览器面板新标签打开：${target}`
              : `已导航到：${target}`;
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: { ok: true, summary, exit_code: 0 },
          });
          return;
        }
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
