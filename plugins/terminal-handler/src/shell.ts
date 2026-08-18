import {
  createTiangongBridge,
  createToolProvider,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';

/**
 * 终端插件 TS 壳：声明归插件、原生归宿主。
 *
 * 工具执行策略在插件（本文件）：收到 tool.requested 后经宿主桥接的
 * `terminal.*` 原生服务（PTY）执行并回传结果；PTY 会话与终端面板由宿主
 * 原生容器管理（xterm 渲染不在沙箱内）。
 */

const TOOL_TO_METHOD: Record<string, string> = {
  run_command: 'terminal.runCommand',
  run_shell: 'terminal.runShell',
  terminal_send: 'terminal.send',
};

async function main() {
  const bridge = await createTiangongBridge();
  const tools = createToolProvider(bridge);

  tools.onRequested((invocation: ToolInvocation) => {
    void (async () => {
      const method = TOOL_TO_METHOD[invocation.name];
      if (!method) {
        // 非本插件声明的工具不会路由到这里；防御性拒绝
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'cancelled',
          result: { ok: false, summary: `未知工具 ${invocation.name}`, exit_code: 1 },
        });
        return;
      }
      try {
        const raw = await bridge.call(method, JSON.stringify(invocation.arguments ?? {}));
        const parsed = JSON.parse(raw) as {
          ok?: boolean;
          summary?: string;
          stdout?: string;
          stderr?: string;
          exit_code?: number;
        };
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: {
            ok: parsed.ok ?? true,
            summary: parsed.summary ?? '',
            stdout: parsed.stdout ?? '',
            stderr: parsed.stderr ?? '',
            exit_code: parsed.exit_code ?? (parsed.ok === false ? 1 : 0),
          },
        });
      } catch (error) {
        await tools.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result: {
            ok: false,
            summary: `终端服务调用失败：${String(error)}`,
            exit_code: 1,
          },
        });
      }
    })();
  });
}

void main();
