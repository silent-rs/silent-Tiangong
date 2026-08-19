import { createApp } from 'vue';
import { getShadowHostRuntime } from '@tiangong/plugin-sdk';
import App from './App.vue';
import './shell';

/**
 * 挂载点必须在宿主注入的 shadow 容器内查找（document 查不到 shadow
 * 里的元素），Vue 的字符串选择器走主文档——shadow 下静默挂空，是
 * 面板空白的根因。无容器环境（独立开发调试）回退主文档。
 */
const runtime = getShadowHostRuntime();
const root = runtime?.root ?? document;
const container = root.querySelector<HTMLElement>('#app');
if (container) {
  const app = createApp(App);
  app.mount(container);
}
