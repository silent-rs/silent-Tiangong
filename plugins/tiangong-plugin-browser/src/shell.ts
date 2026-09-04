import {
  createTiangongBridge,
  createToolProvider,
  getShadowHostRuntime,
  openExtensionApp,
  type HostBridge,
  type ToolClosed,
  type ToolInvocation,
  type ToolResolution,
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
  __tiangongBrowserClosedInvocations?: Set<string>;
};

interface WebviewEvent {
  event?: string;
  scope?: string;
  payload?: {
    tab_id?: string;
    url?: string;
  };
}

/** 进行中的工具调用状态：宿主取消/过期（tool.closed）后置 cancelled。 */
const activeInvocations = new Map<string, { cancelled: boolean }>();

function sharedClosedInvocations(): Set<string> {
  const sharedWindow = window as BrowserToolWindow;
  return sharedWindow.__tiangongBrowserClosedInvocations
    ?? (sharedWindow.__tiangongBrowserClosedInvocations = new Set());
}

function closeInvocation(invocationId: string): void {
  sharedClosedInvocations().add(invocationId);
}

function isInvocationCancelled(invocationId: string): boolean {
  return activeInvocations.get(invocationId)?.cancelled === true
    || sharedClosedInvocations().has(invocationId);
}

class ToolResolutionError extends Error {
  constructor(cause: unknown) {
    super(`提交工具结果失败：${String(cause)}`);
    this.name = 'ToolResolutionError';
  }
}

function claimInvocation(invocationId: string): boolean {
  if (sharedClosedInvocations().has(invocationId)) return false;
  const sharedWindow = window as BrowserToolWindow;
  const claims = sharedWindow.__tiangongBrowserToolClaims
    ?? (sharedWindow.__tiangongBrowserToolClaims = new Set());
  if (claims.has(invocationId)) return false;
  claims.add(invocationId);
  return true;
}

function releaseInvocation(invocationId: string): void {
  const claims = (window as BrowserToolWindow).__tiangongBrowserToolClaims;
  window.setTimeout(() => {
    claims?.delete(invocationId);
    activeInvocations.delete(invocationId);
  }, 5_000);
}

/**
 * 提交工具结果；调用已被宿主取消/过期时静默跳过。
 * 宿主保证一次调用只能闭合一次，取消后提交的 resolve 会被拒绝，这里
 * 提前短路，避免取消后仍执行有副作用或已失效的闭合。
 */
async function resolveActive(
  tools: { resolve: (resolution: ToolResolution) => Promise<void> },
  invocationId: string,
  resolution: Omit<ToolResolution, 'invocation_id'>,
): Promise<void> {
  if (isInvocationCancelled(invocationId)) return;
  try {
    await tools.resolve({ invocation_id: invocationId, ...resolution });
  } catch (error) {
    if (isInvocationCancelled(invocationId)) return;
    throw new ToolResolutionError(error);
  }
}

/** 打开浏览器插件 App（app.open 宿主原语，聚焦本进程内的会话实例）。 */
async function requestOpenInstance(
  bridge: HostBridge,
  sessionId: string,
  instanceId?: string,
  showPanel = true,
): Promise<void> {
  try {
    await openExtensionApp(bridge, { sessionId, instanceId, showPanel });
  } catch (error) {
    console.error('打开浏览器面板失败:', error);
  }
}

function normalizeNavigationUrl(value: string): string {
  return value.trim().replace(/\/$/, '');
}

/** web_fetch 开始导航时取得真实页面编号，并通过 SDK 登记对应 App 实例。 */
function registerNextNavigation(
  bridge: HostBridge,
  sessionId: string,
  targetUrl: string,
  showPanel: boolean,
): () => void {
  const expectedScope = `webview:browser:${sessionId}`;
  let stopped = false;
  let off = () => {};
  off = bridge.on('webview.event', (raw) => {
    let event: WebviewEvent;
    try {
      event = JSON.parse(raw) as WebviewEvent;
    } catch {
      return;
    }
    const tabId = event.payload?.tab_id;
    const eventUrl = event.payload?.url ?? '';
    if (
      stopped
      || event.event !== 'navigation_started'
      || event.scope !== expectedScope
      || !tabId
      || (targetUrl && eventUrl
        && normalizeNavigationUrl(targetUrl) !== normalizeNavigationUrl(eventUrl))
    ) return;
    stopped = true;
    off();
    void requestOpenInstance(bridge, sessionId, tabId, showPanel);
  });
  return () => {
    if (stopped) return;
    stopped = true;
    off();
  };
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

  // 在当前应用生命周期保留闭合记录，拒绝宿主重放快照中晚于 tool.closed 到达的 requested。
  tools.onClosed((closed: ToolClosed) => {
    const active = activeInvocations.get(closed.invocation_id);
    if (active) active.cancelled = true;
    closeInvocation(closed.invocation_id);
  });

  tools.onRequested((invocation: ToolInvocation) => {
    // multi 模式下每个浏览器顶部标签都会挂载一个页面。宿主事件会送达
    // 所有实例，按 invocation_id 只允许其中一个实例执行工具。
    if (!claimInvocation(invocation.invocation_id)) return;
    activeInvocations.set(invocation.invocation_id, { cancelled: false });
    void (async () => {
      try {
        // browser_close（面板开关，app.* 原语）：带 tab_id 精确关闭一个
        // 页面，不带则收起整个浏览器面板（用户明确要求或任务完成时）。
        if (invocation.name === 'browser_close') {
          if (isInvocationCancelled(invocation.invocation_id)) return;
          const args = (invocation.arguments ?? {}) as { tab_id?: string };
          await bridge.call(
            'app.close',
            JSON.stringify(
              args.tab_id
                ? { session_id: invocation.session_id, instance_id: args.tab_id }
                : { session_id: invocation.session_id, all: true },
            ),
          );
          await resolveActive(tools, invocation.invocation_id, {
            status: 'answered',
            result: {
              ok: true,
              summary: args.tab_id ? `已关闭页面 ${args.tab_id}` : '已收起浏览器面板',
              exit_code: 0,
            },
          });
          return;
        }
        const method = TOOL_METHOD[invocation.name];
        if (!method) {
          await resolveActive(tools, invocation.invocation_id, {
            status: 'cancelled',
            result: { ok: false, summary: `未知工具 ${invocation.name}`, exit_code: 1 },
          });
          return;
        }
        // 发起会话与当前页面作用域一致时先刷新宿主页面快照；其他会话
        // 直接调用原语。可见标签仍统一由 App 拓展区顶部标签维护。
        if (
          (invocation.name === 'browser_open' || invocation.name === 'browser_navigate') &&
          tabsModel.scope === invocation.session_id &&
          typeof (invocation.arguments as { url?: unknown })?.url === 'string'
        ) {
          const target = (invocation.arguments as { url: string }).url;
          if (invocation.name === 'browser_open' || tabsModel.tabs.length === 0) {
            if (isInvocationCancelled(invocation.invocation_id)) return;
            const opened = await tabsModel.newTab(target);
            if (opened && !isInvocationCancelled(invocation.invocation_id)) {
              void requestOpenInstance(bridge, invocation.session_id, opened.id);
            }
          } else {
            if (isInvocationCancelled(invocation.invocation_id)) return;
            await tabsModel.navigate(target);
          }
          const summary =
            invocation.name === 'browser_open'
              ? `已在浏览器面板新标签打开：${target}`
              : `已导航到：${target}`;
          await resolveActive(tools, invocation.invocation_id, {
            status: 'answered',
            result: { ok: true, summary, exit_code: 0 },
          });
          return;
        }
        // 会话绑定（对齐终端插件）：Agent 打开/操作的页面归属发起对话，
        // 与该对话的浏览器面板是同一实例（插件×会话双维度隔离）。
        const invocationArgs = (invocation.arguments as Record<string, unknown>) ?? {};
        if (isInvocationCancelled(invocation.invocation_id)) return;
        const showFetchPanel = invocation.name === 'web_fetch' && invocationArgs.open === true;
        const stopAppRegistration = invocation.name === 'web_fetch'
          ? registerNextNavigation(
            bridge,
            invocation.session_id,
            typeof invocationArgs.url === 'string' ? invocationArgs.url : '',
            showFetchPanel,
          )
          : null;
        const { open: _open, ...webviewArgs } = invocationArgs;
        let raw: string;
        try {
          raw = await bridge.call(
            method,
            JSON.stringify({
              ...webviewArgs,
              session_id: invocation.session_id,
            }),
          );
        } finally {
          stopAppRegistration?.();
        }
        if (isInvocationCancelled(invocation.invocation_id)) return;
        const parsed = JSON.parse(raw) as {
          view_id?: string;
          tabs?: Array<{ id?: string; url?: string; title?: string }>;
          active_tab_id?: string | null;
          result?: string | null;
        };
        if (invocation.name === 'browser_open' && parsed.active_tab_id) {
          void requestOpenInstance(bridge, invocation.session_id, parsed.active_tab_id);
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
        await resolveActive(tools, invocation.invocation_id, {
          status: 'answered',
          result: { ok: true, summary, exit_code: 0 },
        });
      } catch (error) {
        if (isInvocationCancelled(invocation.invocation_id)) return;
        if (error instanceof ToolResolutionError) {
          console.error(error.message);
          return;
        }
        await resolveActive(tools, invocation.invocation_id, {
          status: 'answered',
          result: { ok: false, summary: `webview 调用失败：${String(error)}`, exit_code: 1 },
        });
      }
    })().finally(() => releaseInvocation(invocation.invocation_id));
  });
}

void main();
