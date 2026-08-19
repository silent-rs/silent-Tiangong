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

async function bootstrap() {
  const bridge = await createTiangongBridge();
  bridgeRef = bridge;

  const root = getShadowHostRuntime()?.root ?? document;
  const host = root.querySelector<HTMLElement>('#terminal-root');
  if (!host) return;

  terminalView = createTerminalView(host, bridge);
  // 附着最近会话；无会话时创建默认交互 shell（cmd 缺省即登录 shell，
  // 与原生终端「打开即可输入」的体验一致）。
  const latest = [...terminalSessions.values()].pop();
  if (latest) {
    terminalView.attach(latest.session_id);
    return;
  }
  try {
    const spawned = (await sidecarCall(bridge, 'terminalSpawn', {})) as {
      session_id?: string;
    };
    if (spawned.session_id) {
      terminalSessions.set(spawned.session_id, {
        session_id: spawned.session_id,
        scope_id: 'ui',
        created_at: Date.now(),
      });
      terminalView.attach(spawned.session_id);
    }
  } catch (error) {
    console.warn('[terminal] 默认会话创建失败', error);
  }
}

void bootstrap();

export { bridgeRef, terminalView, sidecarCall };
