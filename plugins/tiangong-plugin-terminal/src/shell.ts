import {
  createTiangongBridge,
  createToolProvider,
  openExtensionApp,
  type HostBridge,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';

/**
 * 终端插件 TS 壳（完全体：PTY 在本插件 sidecar，宿主零终端代码）。
 *
 * - 工具执行：tool.requested → sidecar（terminalSpawn/Write）→ tool.resolve
 * - 会话输出：sidecar 经 terminal.output 通知推流 → TerminalView（xterm.js）
 * - 会话退出：terminal.exit 通知
 */

/** 工具名 → sidecar 操作映射。 */
const TOOL_TO_OPERATION: Record<string, string> = {
  run_command: 'runCommand',
  run_shell: 'runShell',
  terminal_send: 'terminalSend',
};

// multi 模式下每个终端顶部标签都挂载一个 shell 实例，宿主事件只由其中
// 一个处理；会话→最近打开实例的记录挂主文档 window 共享（与浏览器插件
// 的 invocation 去重同模式），保证 open/close 可落到不同实例。
type TerminalToolWindow = Window & {
  __tiangongTerminalLastOpened?: Map<string, string>;
  __tiangongTerminalToolClaims?: Set<string>;
};

function lastOpenedBySession(): Map<string, string> {
  const shared = window as TerminalToolWindow;
  return shared.__tiangongTerminalLastOpened
    ?? (shared.__tiangongTerminalLastOpened = new Map());
}

function claimInvocation(invocationId: string): boolean {
  const sharedWindow = window as TerminalToolWindow;
  const claims = sharedWindow.__tiangongTerminalToolClaims
    ?? (sharedWindow.__tiangongTerminalToolClaims = new Set());
  if (claims.has(invocationId)) return false;
  claims.add(invocationId);
  return true;
}

function releaseInvocation(invocationId: string): void {
  const claims = (window as TerminalToolWindow).__tiangongTerminalToolClaims;
  window.setTimeout(() => claims?.delete(invocationId), 5_000);
}

/** 插件内共享的 PTY 会话注册表（工具执行与 UI 共用）。 */
export interface TerminalSessionInfo {
  session_id: string;
  /** 关联的会话（作用域）ID。 */
  scope_id: string;
  created_at: number;
}

const sessions = new Map<string, TerminalSessionInfo>();

async function callSidecar(operation: string, payload: unknown): Promise<Record<string, unknown>> {
  return {};
}

async function main(bridgePromise: Awaitable<HostBridge>) {
  const bridge = await bridgePromise;
  const tools = createToolProvider(bridge);

  tools.onRequested((invocation: ToolInvocation) => {
    if (!claimInvocation(invocation.invocation_id)) return;
    void (async () => {
      // 面板开关走 app.* 宿主原语（不经 sidecar）。实例编号由插件自行
      // 生成与管理（app.open 幂等重开同一实例），Agent 无需传入——结果
      // 中告知实际面板编号，多面板需要精确关闭时可引用（tab_id 可选），
      // 缺省关闭本会话最近由 Agent 打开的面板（PTY 继续运行）。
      if (invocation.name === 'terminal_open' || invocation.name === 'terminal_close') {
        const isClose = invocation.name === 'terminal_close';
        const closeArgs = (invocation.arguments ?? {}) as { tab_id?: string };
        const closeTarget = closeArgs.tab_id
          ?? lastOpenedBySession().get(invocation.session_id);
        if (isClose && !closeTarget) {
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: {
              ok: false,
              summary: '本会话没有 Agent 打开的终端面板可关闭（如需关闭指定面板可传 tab_id）',
              exit_code: 1,
            },
          });
          return;
        }
        const instanceId = isClose
          ? closeTarget!
          : `terminal-${crypto.randomUUID()}`;
        try {
          if (isClose) {
            await bridge.call(
              'app.close',
              JSON.stringify({
                session_id: invocation.session_id,
                instance_id: instanceId,
              }),
            );
          } else {
            await openExtensionApp(bridge, {
              sessionId: invocation.session_id,
              instanceId,
              showPanel: true,
            });
          }
          if (isClose) {
            const opened = lastOpenedBySession();
            if (opened.get(invocation.session_id) === instanceId) {
              opened.delete(invocation.session_id);
            }
          } else {
            lastOpenedBySession().set(invocation.session_id, instanceId);
          }
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: {
              ok: true,
              summary: isClose
                ? `已关闭终端面板（${instanceId}，后台命令继续运行）`
                : `已打开终端面板（${instanceId}）`,
              exit_code: 0,
            },
          });
        } catch (error) {
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: {
              ok: false,
              summary: `终端面板操作失败：${String(error)}`,
              exit_code: 1,
            },
          });
        }
        return;
      }
      const operation = TOOL_TO_OPERATION[invocation.name];
      if (!operation) {
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'cancelled',
          result: { ok: false, summary: `未知工具 ${invocation.name}`, exit_code: 1 },
        });
        return;
      }
      try {
        const result = await executeTool(bridge, operation, invocation);
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result,
        });
      } catch (error) {
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: {
            ok: false,
            summary: `终端执行失败：${String(error)}`,
            exit_code: 1,
          },
        });
      }
    })().finally(() => releaseInvocation(invocation.invocation_id));
  });
}

type Awaitable<T> = Promise<T> | T;

/** 供 TerminalView 复用的 sidecar 直调封装。 */
export async function sidecarCall(
  bridge: HostBridge,
  operation: string,
  payload: unknown,
): Promise<Record<string, unknown>> {
  const raw = await bridge.call(`sidecar.${operation}`, JSON.stringify(payload ?? {}));
  return JSON.parse(raw) as Record<string, unknown>;
}

/** PTY 会话注册表访问（UI 与工具共享）。 */
export const terminalSessions = sessions;

async function executeTool(
  bridge: HostBridge,
  operation: string,
  invocation: ToolInvocation,
): Promise<{ ok: boolean; summary: string; stdout?: string; exit_code: number }> {
  const args = (invocation.arguments ?? {}) as Record<string, unknown>;
  if (operation === 'runCommand' || operation === 'runShell') {
    // 创建 PTY 会话执行命令/脚本；工具结果由 exit 通知驱动（简化版：
    // spawn 成功即返回会话信息，完整版等待 exit 或超时聚合输出）。
    // scope_id 绑定宿主会话：终端面板跟随会话切换时能恢复工具会话。
    // 终端归属由插件路由，结果中告知实际执行的终端（PTY 会话编号）。
    const spawnPayload = {
      scope_id: invocation.session_id,
      ...(operation === 'runShell'
        ? { script: args.script, cwd: args.cwd }
        : { cmd: args.cmd, args: args.args, cwd: args.cwd }),
    };
    const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
      ...spawnPayload,
    })) as { session_id?: string };
    if (!spawned.session_id) {
      return { ok: false, summary: 'PTY 会话创建失败', exit_code: 1 };
    }
    sessions.set(spawned.session_id, {
      session_id: spawned.session_id,
      scope_id: invocation.session_id,
      created_at: Date.now(),
    });
    lastOpenedBySession().set(invocation.session_id, spawned.session_id);
    await openExtensionApp(bridge, {
      sessionId: invocation.session_id,
      instanceId: spawned.session_id,
      showPanel: true,
    });
    return {
      ok: true,
      summary: `命令已交由终端 ${spawned.session_id} 执行（输出见终端面板）`,
      exit_code: 0,
    };
  }
  // terminalSend：默认写入最近创建的活跃会话；指定 terminal_id 时定向
  // 发送，结果始终告知实际送达的终端。
  const requested = typeof args.terminal_id === 'string' ? args.terminal_id : '';
  const target = (requested && sessions.get(requested))
    || [...sessions.values()].pop();
  if (!target) {
    return { ok: false, summary: '没有活跃的终端会话可发送输入', exit_code: 1 };
  }
  await sidecarCall(bridge, 'terminalWrite', {
    session_id: target.session_id,
    data: String(args.input ?? ''),
  });
  return {
    ok: true,
    summary: `已发送输入到终端 ${target.session_id}`,
    exit_code: 0,
  };
}

void main(createTiangongBridge());
