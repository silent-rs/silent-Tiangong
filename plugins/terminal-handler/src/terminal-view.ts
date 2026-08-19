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
  /** 当前网格尺寸（启动 PTY 会话时传入，避免首绘按错误宽度换行）。 */
  size(): { cols: number; rows: number };
}

export function createTerminalView(
  host: HTMLElement,
  bridge: TerminalViewBridge,
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
  // 容器 0 尺寸（未布局/隐藏）时 fit 抛错；忽略等 ResizeObserver 首次回调再算
  try { fit.fit(); } catch { /* 尚未布局 */ }

  // 诊断角标：实测 cols/rows 与容器尺寸（调试显示异常时直接可见）
  const diag = document.createElement('div');
  diag.setAttribute('data-terminal-diag', '');
  diag.style.cssText = 'position:absolute;right:8px;bottom:6px;color:#585b70;font-size:11px;font-family:ui-monospace,monospace;pointer-events:none;z-index:9;';
  const updateDiag = () => {
    diag.textContent = `cols=${terminal.cols} rows=${terminal.rows} host=${host.clientWidth}x${host.clientHeight}`;
  };
  if (getComputedStyle(host).position === 'static') host.style.position = 'relative';
  host.appendChild(diag);
  updateDiag();

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

  // 视口回到最新行（shadow 容器内 xterm 的视口刷新异步，初始可能滞留顶部）
  const scrollToBottomSoon = () => {
    setTimeout(() => terminal.scrollToBottom(), 0);
  };

  const handle = {
    attach(sessionId: string) {
      attached = sessionId;
      terminal.reset();
      terminal.focus();
      try { fit.fit(); } catch { /* 尚未布局 */ }
      syncSize();
      updateDiag();
      // shadow 容器内字体/布局测量可能晚一拍才稳定：延迟复测兜底，
      // 避免会话以默认 80 列启动后不再修正（ls 列数异常的根源）。
      setTimeout(() => {
        try { fit.fit(); } catch { return; }
        syncSize();
        updateDiag();
      }, 120);
      setTimeout(() => {
        try { fit.fit(); } catch { return; }
        syncSize();
        updateDiag();
      }, 400);
      scrollToBottomSoon();
    },
    size() {
      return { cols: terminal.cols, rows: terminal.rows };
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
      try { fit.fit(); } catch { return; }
      syncSize();
      updateDiag();
      scrollToBottomSoon();
    });
    observer.observe(host);
    disposers.push(() => observer.disconnect());
  }

  return handle;
}
