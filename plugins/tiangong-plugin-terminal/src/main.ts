import { createTiangongBridge, getShadowHostRuntime, type HostBridge } from '@tiangong/plugin-sdk';
import { createTerminalView, type TerminalViewHandle } from './terminal-view';
import {
  closeFrontendTerminalTab,
  registerFrontendTerminalTab,
  sidecarCall,
  terminalSessions,
} from './shell';

/**
 * 入口：初始化桥接 → 终端视图（纯显示与输入）。
 * 工具执行由 sidecar 握手注册后交给宿主直连，页面不参与调度。
 * shadow 容器把页面元素注入 ShadowRoot：挂载点必须经宿主注入的
 * pluginRoot 查询（document 查不到 shadow 内元素）。
 *
 * 终端跟会话走（对齐内置终端面板）：宿主上下文的会话变化驱动切换，
 * 每个会话一个独立 PTY；切走再切回保持原终端，明确关闭 App 标签时
 * 结束旧终端并清除恢复记录。
 */

let bridgeRef: HostBridge | null = null;
let terminalView: TerminalViewHandle | null = null;

/** 无活跃会话时的全局终端作用域（cwd 为全局工作区）。 */
const GLOBAL_SCOPE = '__global__';
/** 当前跟随的会话作用域。 */
let currentScope = '';
/** 当前 App 实例；工具创建的实例 ID 同时就是要精确附着的 PTY ID。 */
let currentAppInstance = '';
/** 切换防抖序号：异步恢复中会话再变时丢弃过期结果。 */
let switchTicket = 0;
const switchTasks = new Set<Promise<void>>();

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
  requestedSessionId: string | undefined,
  isCurrent: () => boolean,
): Promise<{ sessionId: string; boot?: string } | null> {
  // 等容器布局完成后按真实尺寸启动：shell 首次绘制（提示符/宽度计算）
  // 依赖正确的 cols/rows，错误宽度会导致换行错乱、光标被顶到底部。
  await waitSized(host);
  if (!isCurrent()) return null;
  const size = view.size();
  const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
    cols: size.cols,
    rows: size.rows,
    scope_id: scopeId,
    ...(requestedSessionId ? { session_id: requestedSessionId } : {}),
    ...(workspace ? { cwd: workspace } : {}),
  })) as {
    session_id?: string;
    boot_output?: string;
  };
  if (!spawned.session_id) throw new Error('sidecar 未返回会话 ID');
  const sessionId = String(spawned.session_id);
  terminalSessions.set(sessionId, {
    session_id: sessionId,
    scope_id: scopeId,
    created_at: Date.now(),
  });
  // 首批输出（提示符）随响应返回：冷启动窗口通知监听未就绪会丢首帧
  //（黑屏根因），基线渲染不依赖通知时序。
  return { sessionId, boot: spawned.boot_output };
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
  requestedSessionId: string | undefined,
  isActive: () => boolean,
): Promise<void> {
  const ticket = ++switchTicket;
  const isCurrent = () => isActive() && ticket === switchTicket;
  currentScope = scopeId;
  // App 实例编号就是其 PTY 编号。先完成逻辑附着，后续 find/ensure 即使
  // 因 sidecar 冷启动稍慢，实时输出也能直接进入正确的 xterm，不留空光标。
  if (requestedSessionId) view.prepare(requestedSessionId);

  try {
    // 工具在打开 App 前已经创建并登记了精确 PTY。直接用幂等 spawn
    // 取回该终端的完整当前内容，避免前台再经过通用恢复查找而停在占位。
    const knownSession = requestedSessionId
      ? terminalSessions.get(requestedSessionId)
      : undefined;
    if (knownSession?.scope_id === scopeId) {
      const spawned = await spawnDefault(
        bridge,
        view,
        host,
        scopeId,
        workspace,
        requestedSessionId,
        isCurrent,
      );
      if (!spawned || !isCurrent()) return;
      view.attach(spawned.sessionId, spawned.boot);
      return;
    }

    if (!isCurrent()) return;
    const found = (await sidecarCall(bridge, 'terminalFind', {
      scope_id: scopeId,
      ...(requestedSessionId ? { session_id: requestedSessionId } : {}),
    })) as {
      session_id?: string;
      history?: string;
    };
    if (found.session_id) {
      if (!isCurrent()) return;
      terminalSessions.set(found.session_id, {
        session_id: found.session_id,
        scope_id: scopeId,
        created_at: Date.now(),
      });
      view.attach(found.session_id, found.history);
      return;
    }
    // 新建 shell（无存活会话——sidecar 重启过）：磁盘历史（若有）作为
    // 回填基线显示在上方（对齐内置终端的应用重启恢复）。
    await spawnAndAttach(found.history || undefined);
  } catch (error) {
    if (!isCurrent() || isDisposedBridgeError(error)) return;
    console.warn('[terminal] 恢复会话失败，转新建：', error);
    try {
      await spawnAndAttach();
    } catch (spawnError) {
      // 容器随面板重建/开发模式严格模式自检销毁：本实例自然终止，
      // 重建后的实例会重新初始化，无需提示（否则每次自检都误报）。
      if (!isCurrent() || isDisposedBridgeError(spawnError)) return;
      showBootError(host, String(spawnError));
    }
  }

  /** 新建会话并附着：历史基线在前、新 shell 首批输出在后；无可重放
   * 内容时视图自身显示启动占位（见 terminal-view attach）。 */
  async function spawnAndAttach(baseline?: string): Promise<void> {
    const spawned = await spawnDefault(
      bridge,
      view,
      host,
      scopeId,
      workspace,
      requestedSessionId,
      isCurrent,
    );
    if (!spawned) return;
    const { sessionId, boot } = spawned;
    // 普通容器重建时保留刚创建的 PTY，供新容器直接恢复；同一容器内
    // 的会话切换才回收已经过期的创建结果。
    if (!isActive()) return;
    if (ticket !== switchTicket) {
      terminalSessions.delete(sessionId);
      await sidecarCall(bridge, 'terminalKill', { session_id: sessionId }).catch(() => {});
      return;
    }
    view.attach(sessionId, [baseline, boot].filter(Boolean).join('') || undefined);
  }
}

function isDisposedBridgeError(error: unknown): boolean {
  return String(error).includes('bridge 已随容器卸载');
}

async function bootstrap() {
  const bridge = await createTiangongBridge();
  bridgeRef = bridge;

  const runtime = getShadowHostRuntime();
  const root = runtime?.root ?? document;
  const host = root.querySelector<HTMLElement>('#terminal-root');
  if (!host) return;

  let active = true;
  runtime?.registerCleanup(() => {
    active = false;
    ++switchTicket;
    currentScope = '';
    currentAppInstance = '';
    switchTasks.clear();
    terminalView?.dispose();
    terminalView = null;
    if (bridgeRef === bridge) bridgeRef = null;
  });
  runtime?.registerBeforeClose(() => {
    const scopeId = currentScope;
    const sessionId = terminalView?.sessionId() || currentAppInstance;
    ++switchTicket;
    currentScope = '';
    currentAppInstance = '';
    if (!scopeId || !sessionId) return;
    terminalSessions.delete(sessionId);
    // 用户关闭标签必须立即成功。提交剩余存活集合触发 Terminal GC；失败
    // 不阻断前端移除，下一次标签新建或关闭会重新对账。
    closeFrontendTerminalTab(bridge, scopeId, sessionId);
  });

  // 会话上下文驱动终端跟随：无活跃会话时挂全局终端，会话出现后切换。
  const follow = (
    session: { id?: string; workspace?: string } | undefined,
    app: { instance_id?: string; visible?: boolean } | undefined,
  ) => {
    if (!active || !terminalView) return;
    const scope = session?.id ?? GLOBAL_SCOPE;
    const appInstance = app?.instance_id ?? '';
    if (scope === currentScope && appInstance === currentAppInstance) return;
    currentAppInstance = appInstance;
    const exactSession = appInstance || undefined;
    const task = switchScope(
      bridge,
      terminalView,
      host,
      scope,
      session?.workspace,
      exactSession,
      () => active,
    );
    switchTasks.add(task);
    void task.finally(() => switchTasks.delete(task));
  };
  const applyContext = (context: NonNullable<typeof runtime>['context']) => {
    if (!active) return;
    const scopeId = context.session?.id ?? GLOBAL_SCOPE;
    const appInstance = context.app?.instance_id ?? '';
    registerFrontendTerminalTab(bridge, scopeId, appInstance);
    // 带精确实例编号的后台标签虽不创建 xterm，仍记录其归属；用户未激活
    // 就强制关闭时可登记 GC。无实例编号的通用隐藏壳不会关联任何 PTY。
    if (!terminalView && context.app?.visible === false) {
      currentScope = scopeId;
      currentAppInstance = appInstance;
      return;
    }
    if (!terminalView) {
      terminalView = createTerminalView(host, bridge, {
        onSessionExit(sessionId) {
          terminalSessions.delete(sessionId);
        },
      });
    }
    follow(context.session, context.app);
    if (context.app?.visible) terminalView?.reveal();
  };
  if (runtime) {
    applyContext(runtime.context);
    runtime.registerCleanup(runtime.onContextChange(applyContext));
  } else {
    terminalView = createTerminalView(host, bridge, {
      onSessionExit(sessionId) {
        terminalSessions.delete(sessionId);
      },
    });
    follow(undefined, undefined);
  }
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
