import { createTiangongBridge, getShadowHostRuntime, type HostBridge } from '@tiangong/plugin-sdk';
import { createTerminalView, type TerminalViewHandle } from './terminal-view';
import { sidecarCall, terminalSessions } from './shell';

/**
 * 入口：初始化桥接 → 工具壳（shell.ts 静态导入即完成订阅）→ 终端视图。
 * shadow 容器把页面元素注入 ShadowRoot：挂载点必须经宿主注入的
 * pluginRoot 查询（document 查不到 shadow 内元素）。
 */

let bridgeRef: HostBridge | null = null;
let terminalView: TerminalViewHandle | null = null;

/** 等容器完成布局（有实际尺寸）再启动会话；容器异常时最多等 500ms。 */
function waitSized(host: HTMLElement): Promise<void> {
  return new Promise((resolve) => {
    if (host.clientWidth > 0 && host.clientHeight > 0) {
      resolve();
      return;
    }
    const timer = setTimeout(() => {
      observer.disconnect();
      resolve();
    }, 500);
    const observer = new ResizeObserver(() => {
      if (host.clientWidth > 0 && host.clientHeight > 0) {
        clearTimeout(timer);
        observer.disconnect();
        resolve();
      }
    });
    observer.observe(host);
  });
}

async function bootstrap() {
  const bridge = await createTiangongBridge();
  bridgeRef = bridge;

  const runtime = getShadowHostRuntime();
  const root = runtime?.root ?? document;
  const host = root.querySelector<HTMLElement>('#terminal-root');
  if (!host) return;

  terminalView = createTerminalView(host, bridge);
  runtime?.registerCleanup(() => {
    terminalView?.dispose();
    terminalView = null;
  });
  // 附着最近会话；无会话时创建默认交互 shell（cmd 缺省即登录 shell，
  // 与原生终端「打开即可输入」的体验一致）。
  const latest = [...terminalSessions.values()].pop();
  if (latest) {
    terminalView.attach(latest.session_id);
    return;
  }
  try {
    // 等容器布局完成后按真实尺寸启动：shell 首次绘制（提示符/宽度计算）
    // 依赖正确的 cols/rows，错误宽度会导致换行错乱、光标被顶到底部。
    await waitSized(host);
    const size = terminalView.size();
    const spawned = (await sidecarCall(bridge, 'terminalSpawn', {
      cols: size.cols,
      rows: size.rows,
    })) as {
      session_id?: string;
    };
    if (spawned.session_id) {
      terminalSessions.set(spawned.session_id, {
        session_id: spawned.session_id,
        scope_id: 'ui',
        created_at: Date.now(),
      });
      terminalView.attach(spawned.session_id);
    } else {
      showBootError(host, 'sidecar 未返回会话 ID');
    }
  } catch (error) {
    // 容器随面板重建/开发模式严格模式自检销毁：本实例自然终止，
    // 重建后的实例会重新初始化，无需提示（否则每次自检都误报）。
    if (String(error).includes('已随容器卸载')) return;
    showBootError(host, String(error));
  }
}

/** 会话创建失败时把原因显示在终端区域（黑框静默失败无法排查）。 */
function showBootError(host: HTMLElement, message: string) {
  console.warn('[terminal] 默认会话创建失败:', message);
  host.insertAdjacentHTML(
    'beforeend',
    `<div style="color:#f87171;font-family:ui-monospace,monospace;font-size:12px;padding:8px">终端会话创建失败：${message}</div>`,
  );
}

void bootstrap();

export { bridgeRef, terminalView, sidecarCall };
