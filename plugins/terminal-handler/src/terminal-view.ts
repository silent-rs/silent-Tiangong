/**
 * 终端视图（xterm.js）：挂载于 shadow 容器，完全在插件沙箱内渲染。
 *
 * - 输出：订阅 sidecar 事件（terminal.output）写入 xterm；
 * - 输入：xterm onData → sidecar.terminalWrite；
 * - 会话：terminal.exit 通知标记结束。
 */
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

export interface TerminalViewBridge {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}

export interface TerminalViewHandle {
  dispose(): void;
  /** 写入输入到指定 PTY 会话。 */
  attach(sessionId: string): void;
}

export function createTerminalView(
  host: HTMLElement,
  bridge: TerminalViewBridge,
): TerminalViewHandle {
  const terminal = new Terminal({
    fontFamily: 'var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)',
    fontSize: 13,
    cursorBlink: true,
    convertEol: true,
    theme: {
      background: 'transparent',
    },
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.open(host);
  fit.fit();

  let attached: string | null = null;
  let disposers: Array<() => void> = [];

  // xterm 尺寸同步到 PTY（rows/cols 不一致会导致换行与全屏应用错乱）
  const syncSize = () => {
    if (!attached) return;
    void bridge
      .call('sidecar.terminalResize', JSON.stringify({
        session_id: attached,
        rows: terminal.rows,
        cols: terminal.cols,
      }))
      .catch(() => {});
  };

  const handle = {
    attach(sessionId: string) {
      attached = sessionId;
      terminal.reset();
      terminal.focus();
      fit.fit();
      syncSize();
    },
    dispose() {
      disposers.forEach((stop) => stop());
      disposers = [];
      terminal.dispose();
    },
  };

  terminal.onData((data) => {
    if (!attached) return;
    void bridge
      .call('sidecar.terminalWrite', JSON.stringify({ session_id: attached, data }))
      .catch((error) => console.warn('[terminal] 写入失败', error));
  });

  // 输出流订阅（sidecar 事件经宿主 bridge_event 转发）
  disposers.push(
    bridge.on('sidecar.event', (payload) => {
      let event: { channel?: string; payload?: string };
      try {
        event = JSON.parse(payload);
      } catch {
        return;
      }
      if (event.channel === 'terminal.output') {
        try {
          const output = JSON.parse(event.payload ?? '{}') as {
            session_id?: string;
            data?: string;
          };
          if (output.session_id === attached && output.data) {
            terminal.write(output.data);
          }
        } catch {
          /* 忽略坏帧 */
        }
      } else if (event.channel === 'terminal.exit') {
        try {
          const exit = JSON.parse(event.payload ?? '{}') as {
            session_id?: string;
            exit_code?: number;
          };
          if (exit.session_id === attached) {
            terminal.write(
              `\r\n\x1b[90m[进程已退出，退出码 ${exit.exit_code ?? '未知'}]\x1b[0m\r\n`,
            );
          }
        } catch {
          /* 忽略坏帧 */
        }
      }
    }),
  );

  // 容器尺寸自适应（环境不支持 ResizeObserver 时跳过，如测试环境）
  if (typeof ResizeObserver !== 'undefined') {
    const observer = new ResizeObserver(() => {
      fit.fit();
      syncSize();
    });
    observer.observe(host);
    disposers.push(() => observer.disconnect());
  }

  return handle;
}
