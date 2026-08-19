import { createApp } from 'vue';
import App from './App.vue';
import { createTiangongBridge, type HostBridge } from '@tiangong/plugin-sdk';
import { createTerminalView, type TerminalViewHandle } from './terminal-view';
import { sidecarCall, terminalSessions } from './shell';

/**
 * 入口：初始化桥接 → 工具壳（shell.ts 逻辑内联于此触发）→ 终端视图。
 * extension.tab shadow 容器挂载后渲染 xterm。
 */

let bridgeRef: HostBridge | null = null;
let terminalView: TerminalViewHandle | null = null;

async function bootstrap() {
  const bridge = await createTiangongBridge();
  bridgeRef = bridge;

  // 挂载终端视图（宿主容器即本插件页面根节点）
  const host = document.getElementById('terminal-root');
  if (host) {
    terminalView = createTerminalView(host, bridge);
    // 附着最近会话（无会话时等待工具创建后手动 attach）
    const latest = [...terminalSessions.values()].pop();
    if (latest) terminalView.attach(latest.session_id);
    else terminalView.attach('pending');
  }
}

// 工具壳：shell.ts 已经顶部静态导入，模块加载即完成工具订阅；
// 不用动态 import（其内联包装会引入 import.meta，shadow 容器无法执行）。

void bootstrap();

const app = createApp(App);
app.mount('#app');
export { bridgeRef, terminalView, sidecarCall };
