import { createTiangongBridge, getShadowHostRuntime, type HostBridge } from '@tiangong/plugin-sdk';
import { createTerminalView, type TerminalViewHandle } from './terminal-view';
import { sidecarCall, terminalSessions } from './shell';

/**
 * 入口：初始化桥接 → 工具壳（shell.ts 静态导入即完成订阅）→ 终端视图。
 * shadow 容器把页面元素注入 ShadowRoot：挂载点必须经宿主注入的
 * pluginRoot 查询（document 查不到 shadow 内元素）。
 *
 * 终端跟会话走（对齐内置终端面板）：宿主上下文的会话变化驱动切换，
 * 每个会话一个独立 PTY；切走再切回、关闭面板再打开都恢复原终端
 * （进程保留在 sidecar，输出历史重放）。
 */

let bridgeRef: HostBridge | null = null;
let terminalView: TerminalViewHandle | null = null;

/** 无活跃会话时的全局终端作用域（cwd 为全局工作区）。 */
const GLOBAL_SCOPE = '__global__';
/** 当前跟随的会话作用域。 */
let currentScope = '';
/** 切换防抖序号：异步恢复中会话再变时丢弃过期结果。 */
let switchTicket = 0;

/** 等容器完成布局（有实际尺寸）再启动会话；容器异常时最多等 500ms。 */
function waitSized(host: HTMLElement): Promise<void> {
  return new Promise((resolve) => {
    if (host.clientWidth > 0 && host.clientHeight > 0) {
      resolve();
      return;
    }
    const timer = setTimeout(() => {
      observer.disconnect();
      resolve();
    }, 500);
    const observer = new ResizeObserver(() => {
      if (host.clientWidth > 0 && host.clientHeight > 0) {
        clearTimeout(timer);
        observer.disconnect();
        resolve();
      }
    });
    observer.observe(host);
  });
}

/** 新建会话的默认交互 shell（cmd 缺省即登录 shell），cwd 落在会话工作目录。 */
async function spawnDefault(
  bridge: HostBridge,
  view: TerminalViewHandle,
  host: HTMLElement,
  scopeId: string,
  workspace: string | undefined,
): Promise<string> {
  // 等容器布局完成后按真实尺寸启动：shell 首次绘制（提示符/宽度计算）
  // 依赖正确的 cols/rows，错误宽度会导致换行错乱、光标被顶到底部。
  await waitSized(host);
  const size = view.size();
  const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
    cols: size.cols,
    rows: size.rows,
    scope_id: scopeId,
    ...(workspace ? { cwd: workspace } : {}),
  })) as {
    session_id?: string;
  };
  if (!spawned.session_id) throw new Error('sidecar 未返回会话 ID');
  const sessionId = String(spawned.session_id);
  terminalSessions.set(sessionId, {
    session_id: sessionId,
    scope_id: scopeId,
    created_at: Date.now(),
  });
  return sessionId;
}

/**
 * 跟随会话切换终端：
 * 1. 本视图已知的该会话终端直接附着（内存缓存完整重放）；
 * 2. sidecar 仍有该会话的存活终端时恢复（UI 重开场景，历史由 sidecar 缓冲重放）；
 * 3. 都没有则新建（cwd 为该会话工作目录，无会话时为全局工作区）。
 */
async function switchScope(
  bridge: HostBridge,
  view: TerminalViewHandle,
  host: HTMLElement,
  scopeId: string,
  workspace: string | undefined,
): Promise<void> {
  const ticket = ++switchTicket;
  currentScope = scopeId;

  const local = [...terminalSessions.values()]
    .filter((session) => session.scope_id === scopeId)
    .sort((a, b) => b.created_at - a.created_at)[0];
  if (local) {
    view.attach(local.session_id);
    return;
  }

  try {
    const found = (await sidecarCall(bridge, 'terminalFind', { scope_id: scopeId })) as {
      session_id?: string;
      history?: string;
    };
    if (found.session_id) {
      if (ticket !== switchTicket) return;
      terminalSessions.set(found.session_id, {
        session_id: found.session_id,
        scope_id: scopeId,
        created_at: Date.now(),
      });
      view.attach(found.session_id, found.history);
      return;
    }
    // 无存活会话（sidecar 重启过）：新建 shell，磁盘历史作为回填基线
    // 显示在上方（对齐内置终端的应用重启恢复）。
    const restored = found.history || undefined;
    const sessionId = await spawnDefault(bridge, view, host, scopeId, workspace);
    if (ticket !== switchTicket) return;
    view.attach(sessionId, restored);
  } catch (error) {
    console.warn('[terminal] 恢复会话失败，转新建：', error);
    try {
      const sessionId = await spawnDefault(bridge, view, host, scopeId, workspace);
      if (ticket !== switchTicket) return;
      view.attach(sessionId);
    } catch (spawnError) {
      // 容器随面板重建/开发模式严格模式自检销毁：本实例自然终止，
      // 重建后的实例会重新初始化，无需提示（否则每次自检都误报）。
      if (String(spawnError).includes('已随容器卸载')) return;
      showBootError(host, String(spawnError));
    }
  }
}

async function bootstrap() {
  const bridge = await createTiangongBridge();
  bridgeRef = bridge;

  const runtime = getShadowHostRuntime();
  const root = runtime?.root ?? document;
  const host = root.querySelector<HTMLElement>('#terminal-root');
  if (!host) return;

  terminalView = createTerminalView(host, bridge, {
    // 会话退出即出注册表：切回该会话时按需新建 shell
    onSessionExit(sessionId) {
      terminalSessions.delete(sessionId);
    },
  });
  runtime?.registerCleanup(() => {
    terminalView?.dispose();
    terminalView = null;
  });

  // 会话上下文驱动终端跟随：无活跃会话时挂全局终端，会话出现后切换。
  const follow = (session: { id?: string; workspace?: string } | undefined) => {
    if (!terminalView) return;
    const scope = session?.id ?? GLOBAL_SCOPE;
    if (scope === currentScope) return;
    void switchScope(bridge, terminalView, host, scope, session?.workspace);
  };
  follow(runtime?.context.session);
  runtime?.onContextChange((context) => follow(context.session));
}

/** 会话创建失败时把原因显示在终端区域（黑框静默失败无法排查）。 */
function showBootError(host: HTMLElement, message: string) {
  console.warn('[terminal] 默认会话创建失败:', message);
  host.insertAdjacentHTML(
    'beforeend',
    `<div style="color:#f87171;font-family:ui-monospace,monospace;font-size:12px;padding:8px">终端会话创建失败：${message}</div>`,
  );
}

void bootstrap();

export { bridgeRef, terminalView, sidecarCall };
