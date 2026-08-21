import {
  createTiangongBridge,
  createToolProvider,
  getShadowHostRuntime,
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
// 一个处理；共享注册表挂主文档 window，供前后台实例共同使用。
type TerminalToolWindow = Window & {
  __tiangongTerminalToolClaims?: Set<string>;
  __tiangongTerminalSessions?: Map<string, TerminalSessionInfo>;
  __tiangongTerminalWorkspaces?: Map<string, string>;
};

function workspaceRegistry(): Map<string, string> {
  const shared = window as TerminalToolWindow;
  return shared.__tiangongTerminalWorkspaces
    ?? (shared.__tiangongTerminalWorkspaces = new Map());
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

function sessionRegistry(): Map<string, TerminalSessionInfo> {
  const shared = window as TerminalToolWindow;
  return shared.__tiangongTerminalSessions
    ?? (shared.__tiangongTerminalSessions = new Map());
}

const sessions = sessionRegistry();

async function main(bridgePromise: Awaitable<HostBridge>) {
  const bridge = await bridgePromise;
  const tools = createToolProvider(bridge);
  const runtime = getShadowHostRuntime();
  const rememberWorkspace = (context: NonNullable<typeof runtime>['context']) => {
    const sessionId = context.session?.id;
    const workspace = context.session?.workspace;
    if (sessionId && workspace) workspaceRegistry().set(sessionId, workspace);
  };
  if (runtime) {
    rememberWorkspace(runtime.context);
    runtime.registerCleanup(runtime.onContextChange(rememberWorkspace));
  }

  tools.onRequested((invocation: ToolInvocation) => {
    if (!claimInvocation(invocation.invocation_id)) return;
    void (async () => {
      // terminal_open 由插件创建终端并返回编号；terminal_close 必须携带
      // 该编号精确关闭。run_command/run_shell 不需要 Agent 提供编号。
      if (invocation.name === 'terminal_open' || invocation.name === 'terminal_close') {
        const isClose = invocation.name === 'terminal_close';
        const closeArgs = (invocation.arguments ?? {}) as { terminal_id?: string };
        const closeTarget = closeArgs.terminal_id?.trim();
        if (isClose && !closeTarget) {
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: {
              ok: false,
              summary: 'terminal_close 缺少 terminal_id，未关闭任何终端',
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
            // App 已不在前台标签时仍保证对应 PTY 被结束；操作幂等。
            await sidecarCall(bridge, 'terminalClose', {
              scope_id: invocation.session_id,
              session_id: instanceId,
            });
          } else {
            const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
              session_id: instanceId,
              scope_id: invocation.session_id,
              cwd: workspaceRegistry().get(invocation.session_id),
            })) as { session_id?: string };
            if (spawned.session_id !== instanceId) {
              throw new Error('终端 sidecar 未创建指定实例');
            }
            sessions.set(instanceId, {
              session_id: instanceId,
              scope_id: invocation.session_id,
              created_at: Date.now(),
            });
            // terminal_open 面向用户展示：建立标签并展开拓展区聚焦。
            await openExtensionApp(bridge, {
              sessionId: invocation.session_id,
              instanceId,
            });
          }
          if (isClose) sessions.delete(instanceId);
          await tools.resolve({
            invocation_id: invocation.invocation_id,
            status: 'answered',
            result: {
              ok: true,
              summary: isClose
                ? `已关闭终端 ${instanceId}`
                : `已打开终端 ${instanceId}`,
              exit_code: 0,
            },
          });
        } catch (error) {
          if (!isClose) {
            await sidecarCall(bridge, 'terminalKill', { session_id: instanceId }).catch(() => {});
            sessions.delete(instanceId);
          }
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
): Promise<{
  ok: boolean;
  summary: string;
  stdout?: string;
  stderr?: string;
  exit_code: number;
}> {
  const args = (invocation.arguments ?? {}) as Record<string, unknown>;
  if (operation === 'runCommand' || operation === 'runShell') {
    // Agent 只提交执行请求；终端选择、新建、界面打开、命令执行和结果收集
    // 全部由插件完成。优先预留当前会话的空闲长期终端，没有才创建新的。
    const acquired = (await sidecarCall(bridge, 'terminalAcquire', {
      scope_id: invocation.session_id,
    })) as { session_id?: string; reason?: string };
    let sessionId = acquired.session_id;
    const createdNew = !sessionId;
    const workspace = typeof args.cwd === 'string' && args.cwd.trim()
      ? args.cwd
      : workspaceRegistry().get(invocation.session_id);
    if (!sessionId) {
      const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
        scope_id: invocation.session_id,
        cwd: workspace,
        reserve: true,
      })) as { session_id?: string };
      sessionId = spawned.session_id;
    }
    if (!sessionId) {
      return { ok: false, summary: '终端会话创建失败', exit_code: 1 };
    }
    sessions.set(sessionId, {
      session_id: sessionId,
      scope_id: invocation.session_id,
      created_at: Date.now(),
    });

    let executed: {
        stdout?: string;
        stderr?: string;
        exit_code?: number;
        timed_out?: boolean;
        cwd_after?: string;
        interactive_mode?: boolean;
      };
    try {
      // 命令执行静默建立前台标签（不弹面板）：终端对用户可见可关，
      // 实例编号与 PTY 一致，重复调用幂等聚焦同一标签。
      await openExtensionApp(bridge, {
        sessionId: invocation.session_id,
        instanceId: sessionId,
        showPanel: false,
      });
      executed = (await sidecarCall(bridge, 'terminalExec', {
        session_id: sessionId,
        cwd: workspace,
        ...(operation === 'runShell'
          ? {
            script: args.script,
            interactive: args.interactive === true,
          }
          : {
            cmd: args.cmd,
            args: Array.isArray(args.args) ? args.args : [],
          }),
        ...(typeof args.timeout === 'number' ? { timeout: args.timeout } : {}),
      })) as typeof executed;
    } catch (error) {
      await sidecarCall(bridge, 'terminalRelease', { session_id: sessionId }).catch(() => {});
      throw error;
    }
    const exitCode = typeof executed.exit_code === 'number' ? executed.exit_code : -1;
    const cwd = executed.cwd_after ? `，cwd: ${executed.cwd_after}` : '';
    const reason = acquired.reason === 'all_busy'
      ? '当前会话已有终端都在忙'
      : '当前会话没有可用终端';
    const selection = createdNew
      ? `新终端 ${sessionId}（${reason}，没有写入旧终端）`
      : `终端 ${sessionId}（复用空闲终端）`;
    const summary = executed.interactive_mode
      ? `命令已在${selection}进入交互状态`
      : executed.timed_out
        ? `命令在${selection}执行超时${cwd}`
        : exitCode === 0
          ? `命令已在${selection}执行完成${cwd}`
          : `命令已在${selection}结束，退出码 ${exitCode}${cwd}`;
    return {
      ok: true,
      summary,
      stdout: executed.stdout ?? '',
      stderr: executed.stderr ?? '',
      exit_code: exitCode,
    };
  }
  // terminalSend 必须精确指定终端；控制键解码和画面等待由 sidecar 完成。
  const requested = typeof args.terminal_id === 'string' ? args.terminal_id.trim() : '';
  if (!requested) {
    return { ok: false, summary: 'terminal_send 缺少 terminal_id', exit_code: 1 };
  }
  const sent = (await sidecarCall(bridge, 'terminalSend', {
    scope_id: invocation.session_id,
    session_id: requested,
    input: String(args.input ?? ''),
    ...(typeof args.wait === 'number' ? { wait: args.wait } : {}),
  })) as {
    session_id?: string;
    stdout?: string;
    exit_code?: number;
    interactive_mode?: boolean;
  };
  if (!sent.session_id) {
    return { ok: false, summary: '没有活跃的终端会话可发送输入', exit_code: 1 };
  }
  return {
    ok: true,
    summary: `终端 ${sent.session_id} 已更新，当前内容见 stdout`,
    stdout: sent.stdout ?? '',
    exit_code: typeof sent.exit_code === 'number' ? sent.exit_code : 0,
  };
}

void main(createTiangongBridge());
