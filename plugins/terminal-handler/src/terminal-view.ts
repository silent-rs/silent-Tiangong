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
  /** 写入输入到指定 PTY 会话；history 为 UI 重开等缓存丢失场景的重放基线。 */
  attach(sessionId: string, history?: string): void;
  /** 当前网格尺寸（启动 PTY 会话时传入，避免首绘按错误宽度换行）。 */
  size(): { cols: number; rows: number };
}

/** 终端视图配置。 */
export interface TerminalViewOptions {
  /** 会话进程退出回调（含非当前显示的会话）：调用方清理会话注册表。 */
  onSessionExit?(sessionId: string): void;
}

export function createTerminalView(
  host: HTMLElement,
  bridge: TerminalViewBridge,
  options: TerminalViewOptions = {},
): TerminalViewHandle {
  const terminal = new Terminal({
    fontFamily: 'Menlo, Monaco, "Courier New", monospace',
    fontSize: 13,
    cursorBlink: true,
    // PTY 输出已含 \r\n，转换会叠加多余回车导致行错位
    convertEol: false,
    scrollback: 10000,
    theme: {
      background: '#1e1e2e',
      foreground: '#cdd6f4',
      cursor: '#f5e0dc',
      selectionBackground: '#585b7066',
    },
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.open(host);
  const fitToHost = () => {
    try {
      fit.fit();
      return true;
    } catch {
      return false;
    }
  };
  fitToHost();

  let attached: string | null = null;
  let disposers: Array<() => void> = [];
  let lastSyncedSize = '';
  const pendingOutput = new Map<string, string>();
  const pendingTimers = new Set<number>();
  const maxPendingOutput = 256 * 1024;

  // xterm 尺寸同步到 PTY（rows/cols 不一致会导致换行与全屏应用错乱）
  const syncSize = () => {
    if (!attached) return;
    const sizeKey = `${attached}:${terminal.cols}x${terminal.rows}`;
    if (sizeKey === lastSyncedSize) return;
    lastSyncedSize = sizeKey;
    void bridge
      .call('sidecar.terminalResize', JSON.stringify({
        session_id: attached,
        rows: terminal.rows,
        cols: terminal.cols,
      }))
      .catch(() => {
        if (lastSyncedSize === sizeKey) lastSyncedSize = '';
      });
  };

  const scheduleFit = (delay: number) => {
    const timer = window.setTimeout(() => {
      pendingTimers.delete(timer);
      if (!fitToHost()) return;
      syncSize();
    }, delay);
    pendingTimers.add(timer);
  };

  const handle = {
    attach(sessionId: string, history?: string) {
      attached = sessionId;
      lastSyncedSize = '';
      terminal.reset();
      terminal.focus();
      fitToHost();
      syncSize();
      // terminalSpawn 返回前 shell 已可能产生提示符。重放顺序：恢复基线
      // （UI 重开的磁盘历史 / sidecar 内存缓冲）在前、本视图缓存的增量在
      // 后，保留完整控制序列；解析完成后定位到最新一行。
      const pending = pendingOutput.get(sessionId) ?? '';
      pendingOutput.delete(sessionId);
      const buffered = [history ?? '', pending].filter(Boolean).join('');
      if (buffered) {
        terminal.write(buffered, () => terminal.scrollToBottom());
      }
      // 字体与 Shadow 布局可能晚一拍稳定，复测只在网格真的变化时同步。
      scheduleFit(120);
      scheduleFit(400);
    },
    size() {
      return { cols: terminal.cols, rows: terminal.rows };
    },
    dispose() {
      for (const timer of pendingTimers) window.clearTimeout(timer);
      pendingTimers.clear();
      pendingOutput.clear();
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
          if (!output.session_id || !output.data) return;
          if (output.session_id === attached) {
            terminal.write(output.data);
          } else {
            // 非当前显示会话的输出持续缓存（上限截断）：切换回来时
            // 完整重放，保持各会话终端互不丢失。
            const buffered = `${pendingOutput.get(output.session_id) ?? ''}${output.data}`;
            pendingOutput.set(output.session_id, buffered.slice(-maxPendingOutput));
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
          if (exit.session_id) {
            options.onSessionExit?.(exit.session_id);
            pendingOutput.delete(exit.session_id);
          }
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
      if (!fitToHost()) return;
      syncSize();
    });
    observer.observe(host);
    disposers.push(() => observer.disconnect());
  }

  return handle;
}
