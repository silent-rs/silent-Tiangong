import {
  createTiangongBridge,
  createToolProvider,
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

async function main(bridgePromise: Awaitable<HostBridgeLike>) {
  const bridge = await bridgePromise;
  const tools = createToolProvider(bridge);

  tools.onRequested((invocation: ToolInvocation) => {
    void (async () => {
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
    })();
  });
}

/** HostBridge 的最小结构（避免循环依赖 SDK 类型）。 */
interface HostBridgeLike {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}
type Awaitable<T> = Promise<T> | T;

/** 供 TerminalView 复用的 sidecar 直调封装。 */
export async function sidecarCall(
  bridge: { call(method: string, payload: string): Promise<string> },
  operation: string,
  payload: unknown,
): Promise<Record<string, unknown>> {
  const raw = await bridge.call(`sidecar.${operation}`, JSON.stringify(payload ?? {}));
  return JSON.parse(raw) as Record<string, unknown>;
}

/** PTY 会话注册表访问（UI 与工具共享）。 */
export const terminalSessions = sessions;

async function executeTool(
  bridge: { call(method: string, payload: string): Promise<string> },
  operation: string,
  invocation: ToolInvocation,
): Promise<{ ok: boolean; summary: string; stdout?: string; exit_code: number }> {
  const args = (invocation.arguments ?? {}) as Record<string, unknown>;
  if (operation === 'runCommand' || operation === 'runShell') {
    // 创建 PTY 会话执行命令/脚本；工具结果由 exit 通知驱动（简化版：
    // spawn 成功即返回会话信息，完整版等待 exit 或超时聚合输出）。
    const spawnPayload =
      operation === 'runShell'
        ? { script: args.script, cwd: args.cwd }
        : { cmd: args.cmd, args: args.args, cwd: args.cwd };
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
    return {
      ok: true,
      summary: `已在终端会话 ${spawned.session_id} 启动执行（输出见终端面板）`,
      exit_code: 0,
    };
  }
  // terminalSend：写入最近创建的活跃会话
  const latest = [...sessions.values()].pop();
  if (!latest) {
    return { ok: false, summary: '没有活跃的终端会话', exit_code: 1 };
  }
  await sidecarCall(bridge, 'terminalWrite', {
    session_id: latest.session_id,
    data: String(args.input ?? ''),
  });
  return { ok: true, summary: '已发送输入', exit_code: 0 };
}

void main(createTiangongBridge());
