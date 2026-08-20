import {
  createTiangongBridge,
  createToolProvider,
  getShadowHostRuntime,
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

type BrowserToolWindow = Window & {
  __tiangongBrowserToolClaims?: Set<string>;
};

function claimInvocation(invocationId: string): boolean {
  const sharedWindow = window as BrowserToolWindow;
  const claims = sharedWindow.__tiangongBrowserToolClaims
    ?? (sharedWindow.__tiangongBrowserToolClaims = new Set());
  if (claims.has(invocationId)) return false;
  claims.add(invocationId);
  return true;
}

function releaseInvocation(invocationId: string): void {
  const claims = (window as BrowserToolWindow).__tiangongBrowserToolClaims;
  window.setTimeout(() => claims?.delete(invocationId), 5_000);
}

function requestOpenInstance(instanceId: string, sessionId: string): void {
  window.dispatchEvent(new CustomEvent('tiangong:plugin-request-open-instance', {
    detail: {
      plugin_id: 'browser',
      contribution_id: 'browser',
      instance_id: instanceId,
      session_id: sessionId,
    },
  }));
}

async function main() {
  const bridge = await createTiangongBridge();
  await tabsModel.attach(bridge);
  const runtime = getShadowHostRuntime();
  tabsModel.scope = runtime?.context.session?.id ?? '__global__';
  await tabsModel.restore().catch(() => {});
  if (runtime) {
    const stop = runtime.onContextChange((context) => {
      const nextScope = context.session?.id ?? '__global__';
      if (nextScope === tabsModel.scope) return;
      tabsModel.scope = nextScope;
      void tabsModel.restore().catch(() => {});
    });
    runtime.registerCleanup(stop);
  }
  const tools = createToolProvider(bridge);

  tools.onRequested((invocation: ToolInvocation) => {
    // multi 模式下每个浏览器顶部标签都会挂载一个页面。宿主事件会送达
    // 所有实例，按 invocation_id 只允许其中一个实例执行工具。
    if (!claimInvocation(invocation.invocation_id)) return;
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
        // 发起会话与当前页面作用域一致时先刷新宿主页面快照；其他会话
        // 直接调用原语。可见标签仍统一由 App 拓展区顶部标签维护。
        if (
          (invocation.name === 'browser_open' || invocation.name === 'browser_navigate') &&
          tabsModel.scope === invocation.session_id &&
          typeof (invocation.arguments as { url?: unknown })?.url === 'string'
        ) {
          const target = (invocation.arguments as { url: string }).url;
          if (invocation.name === 'browser_open' || tabsModel.tabs.length === 0) {
            const opened = await tabsModel.newTab(target);
            if (opened) {
              requestOpenInstance(opened.id, invocation.session_id);
            }
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
        if (invocation.name === 'browser_open' && parsed.active_tab_id) {
          requestOpenInstance(parsed.active_tab_id, invocation.session_id);
        }
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
    })().finally(() => releaseInvocation(invocation.invocation_id));
  });
}

void main();
