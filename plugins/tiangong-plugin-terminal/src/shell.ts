import type { HostBridge } from '@tiangong/plugin-sdk';

/**
 * 终端插件 TS 壳（视图辅助层）。
 *
 * 工具编排（run_command/run_shell/terminal_open/terminal_send/
 * terminal_close）已下沉 sidecar，并由握手 capabilities 注册 Handler：
 * 宿主直连 sidecar 执行，不经 tool.requested 页面竞争接应。本文件只保留
 * 视图共用的 sidecar 直调封装与 PTY 会话注册表。
 */

/** 插件内共享的 PTY 会话注册表（视图恢复与附着用）。 */
export interface TerminalSessionInfo {
  session_id: string;
  /** 关联的会话（作用域）ID。 */
  scope_id: string;
  created_at: number;
}

// multi 模式下每个终端顶部标签都挂载一个实例，注册表挂主文档 window
// 供各实例共同使用。
type TerminalToolWindow = Window & {
  __tiangongTerminalSessions?: Map<string, TerminalSessionInfo>;
};

function sessionRegistry(): Map<string, TerminalSessionInfo> {
  const shared = window as TerminalToolWindow;
  return shared.__tiangongTerminalSessions
    ?? (shared.__tiangongTerminalSessions = new Map());
}

/** 供 TerminalView 复用的 sidecar 直调封装。 */
export async function sidecarCall(
  bridge: HostBridge,
  operation: string,
  payload: unknown,
): Promise<Record<string, unknown>> {
  const raw = await bridge.call(`sidecar.${operation}`, JSON.stringify(payload ?? {}));
  return JSON.parse(raw) as Record<string, unknown>;
}

/** PTY 会话注册表访问（视图间共享）。 */
export const terminalSessions = sessionRegistry();
