// 工具处理核心：Agent 工具调用与创作页按钮共用同一 plugin-dev.* 宿主通道
// （RFC 0017 D21「页面与 Agent 调用共用同一后端」的落地点）。
import type { HostBridge } from '@tiangong/plugin-sdk';

/** 工具结果（宿主 ts_tools 契约：ok/summary/exit_code 必填）。 */
export interface ToolOutcome {
  ok: boolean;
  summary: string;
  stdout: string;
  stderr: string;
  exit_code: number;
}

export interface InitResult {
  plugin_id: string;
  name: string;
  template: string;
  directory: string;
  files: number;
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

export interface ValidateResult {
  ok: boolean;
  errors: string[];
  warnings: string[];
  id: string | null;
  version: string | null;
  permissions: string[];
  tools: string[];
}

export interface BuildResult {
  duration_ms: number;
  log_tail: string;
  release_dir: string;
}

export interface InstallResult {
  plugin_id: string;
  version: string;
  state: string;
  enabled: boolean;
}

export interface LogsResult {
  path: string;
  lines: string[];
}

export interface RunResult {
  ok: boolean;
  exit_code: number | null;
  duration_ms: number;
  command: string;
  stdout_tail: string;
  stderr_tail: string;
}

async function pluginDevCall<T>(
  bridge: HostBridge,
  operation: string,
  payload: Record<string, unknown>,
): Promise<T> {
  const raw = await bridge.call(`plugin-dev.${operation}`, JSON.stringify(payload));
  return JSON.parse(raw) as T;
}

/** 页面与工具共用的类型化 plugin-dev 操作。 */
export const pluginDev = {
  init: (bridge: HostBridge, request: { template: string; id: string; name?: string }) =>
    pluginDevCall<InitResult>(bridge, 'init', request),
  list: (bridge: HostBridge) => pluginDevCall<ProjectEntry[]>(bridge, 'list', {}),
  validate: (bridge: HostBridge, id: string) =>
    pluginDevCall<ValidateResult>(bridge, 'validate', { id }),
  build: (bridge: HostBridge, id: string) => pluginDevCall<BuildResult>(bridge, 'build', { id }),
  install: (bridge: HostBridge, id: string) =>
    pluginDevCall<InstallResult>(bridge, 'install', { id }),
  logs: (bridge: HostBridge, target: string, lines?: number) =>
    pluginDevCall<LogsResult>(bridge, 'logs', { target, lines }),
  run: (bridge: HostBridge, id: string, args: string[]) =>
    pluginDevCall<RunResult>(bridge, 'run', { id, args }),
  status: (bridge: HostBridge, id: string) =>
    pluginDevCall<Record<string, unknown>>(bridge, 'status', { id }),
};

function ok(summary: string): ToolOutcome {
  return { ok: true, summary, stdout: '', stderr: '', exit_code: 0 };
}

function fail(summary: string): ToolOutcome {
  return { ok: false, summary, stdout: '', stderr: '', exit_code: 1 };
}

/** 分发一次 Agent 工具调用（plugin_init 等）到 plugin-dev.* 通道。 */
export async function handleAgentTool(
  bridge: HostBridge,
  name: string,
  args: Record<string, unknown>,
): Promise<ToolOutcome> {
  try {
    switch (name) {
      case 'plugin_init': {
        const result = await pluginDev.init(bridge, args as never);
        return ok(
          `插件项目已初始化：${result.plugin_id}「${result.name}」（模板 ${result.template}，${result.files} 个文件）\n` +
            `目录：${result.directory}\n` +
            '下一步：浏览项目结构与模板示例 → 按需求填充实现 → plugin_validate → plugin_build → plugin_install',
        );
      }
      case 'plugin_build': {
        const id = String(args.id ?? '');
        const result = await pluginDev.build(bridge, id);
        return ok(
          `构建完成（${(result.duration_ms / 1000).toFixed(1)}s），产物目录：${result.release_dir}\n` +
            '可调 plugin_install 安装（将弹出用户确认）。',
        );
      }
      case 'plugin_install': {
        const id = String(args.id ?? '');
        const result = await pluginDev.install(bridge, id);
        return ok(
          `插件 ${result.plugin_id} v${result.version} 已安装（状态 ${result.state}，${result.enabled ? '已启用' : '未启用'}）。` +
            '若含 extension.tab 贡献，可在拓展区找到新插件的入口。',
        );
      }
      case 'plugin_validate': {
        const id = String(args.id ?? '');
        const result = await pluginDev.validate(bridge, id);
        const lines: string[] = [];
        for (const error of result.errors) lines.push(`错误：${error}`);
        for (const warning of result.warnings) lines.push(`提示：${warning}`);
        if (result.ok && lines.length === 0) {
          lines.push(
            `清单校验通过：${result.id} v${result.version}，权限 [${result.permissions.join('、') || '无'}]` +
              (result.tools.length > 0 ? `，工具 [${result.tools.join('、')}]` : ''),
          );
        }
        return result.ok ? ok(lines.join('\n')) : fail(lines.join('\n'));
      }
      case 'plugin_run': {
        const id = String(args.id ?? '');
        const runArgs = Array.isArray(args.args) ? (args.args as string[]) : [];
        const result = await pluginDev.run(bridge, id, runArgs);
        return result.ok
          ? ok(
              `试运行成功（退出码 ${result.exit_code ?? 0}，${(result.duration_ms / 1000).toFixed(1)}s）\n命令：${result.command}\n\nstdout：\n${result.stdout_tail || '（空）'}` +
                (result.stderr_tail ? `\n\nstderr：\n${result.stderr_tail}` : ''),
            )
          : fail(
              `试运行失败（退出码 ${result.exit_code ?? '超时/无'}，${(result.duration_ms / 1000).toFixed(1)}s）\n命令：${result.command}\n\nstdout：\n${result.stdout_tail || '（空）'}\n\nstderr：\n${result.stderr_tail || '（空）'}`,
            );
      }
      case 'plugin_logs': {
        const target = String(args.target ?? '');
        const lines = typeof args.lines === 'number' ? args.lines : undefined;
        const result = await pluginDev.logs(bridge, target, lines);
        return ok(`日志（${result.path}）尾部：\n${result.lines.join('\n')}`);
      }
      default:
        return fail(`未知工具 ${name}（plugin-creator 提供 plugin_init/build/install/validate/logs）`);
    }
  } catch (error) {
    return fail(`工具 ${name} 失败：${error instanceof Error ? error.message : String(error)}`);
  }
}
