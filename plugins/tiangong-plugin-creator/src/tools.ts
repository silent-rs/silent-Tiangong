// 工具处理核心：Agent 工具调用与创作页共用同一后端。
// 外置化后插件侧只保留安装通道（plugin-dev.install）与只读查询
//（plugin-dev.list/status）；init/validate/build/run/logs 由 Agent 经
// 命令通道执行 @silent-ai/plugin-creator devkit（见 plugin.json 说明书）。
import type { HostBridge } from '@tiangong/plugin-sdk';

/** 工具结果（宿主 ts_tools 契约：ok/summary/exit_code 必填）。 */
export interface ToolOutcome {
  ok: boolean;
  summary: string;
  stdout: string;
  stderr: string;
  exit_code: number;
}

export interface ProjectEntry {
  id: string;
  name: string;
  template: string;
  source_version: string | null;
  release_version: string | null;
  installed_version: string | null;
  created_at: string | null;
}

export interface InstallResult {
  plugin_id: string;
  version: string;
  state: string;
  enabled: boolean;
}

async function pluginDevCall<T>(
  bridge: HostBridge,
  operation: string,
  payload: Record<string, unknown>,
): Promise<T> {
  const raw = await bridge.call(`plugin-dev.${operation}`, JSON.stringify(payload));
  return JSON.parse(raw) as T;
}

/** 宿主侧受限通道（安装与只读查询）。 */
export const pluginDev = {
  install: (bridge: HostBridge, id: string) =>
    pluginDevCall<InstallResult>(bridge, 'install', { id }),
  list: (bridge: HostBridge) => pluginDevCall<ProjectEntry[]>(bridge, 'list', {}),
};

/** devkit 命令模板（页面展示引导，实际由 Agent 经命令通道执行）。 */
export const DEVKIT_VERSION = '1.0.0';
export const DEVKIT_COMMANDS: Record<string, (id: string) => string> = {
  init: (id) => `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} init <模板> ${id} --name <显示名>`,
  validate: (id) => `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} validate ${id}`,
  build: (id) => `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} build ${id}`,
  run: (id) => `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} run ${id} -- <参数>`,
  logs: (id) => `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} logs dev:${id}`,
};

function ok(summary: string): ToolOutcome {
  return { ok: true, summary, stdout: '', stderr: '', exit_code: 0 };
}

function fail(summary: string): ToolOutcome {
  return { ok: false, summary, stdout: '', stderr: '', exit_code: 1 };
}

/** 分发一次 Agent 工具调用（当前仅 plugin_install，其余操作经命令通道）。 */
export async function handleAgentTool(
  bridge: HostBridge,
  name: string,
  args: Record<string, unknown>,
): Promise<ToolOutcome> {
  try {
    if (name !== 'plugin_install') {
      return fail(
        `未知工具 ${name}。开发操作（init/validate/build/run/logs）请经命令通道执行：` +
          `npx -y @silent-ai/plugin-creator@${DEVKIT_VERSION} <命令>`,
      );
    }
    const id = String(args.id ?? '');
    const result = await pluginDev.install(bridge, id);
    return ok(
      `插件 ${result.plugin_id} v${result.version} 已安装（状态 ${result.state}，` +
        `${result.enabled ? '已启用' : '未启用'}）。含 extension.tab 贡献时可在拓展区打开；` +
        '含 mention 声明时可在输入框 @plugin:<id> 点名调用。',
    );
  } catch (error) {
    return fail(`工具 ${name} 失败：${error instanceof Error ? error.message : String(error)}`);
  }
}
