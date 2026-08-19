import { createApp } from 'vue';
import { getShadowHostRuntime } from '@tiangong/plugin-sdk';
import App from './App.vue';

const shadowRuntime = getShadowHostRuntime();
const mountTarget = (shadowRuntime?.root ?? document).querySelector('#app');

if (!mountTarget) {
  throw new Error('交互处理器缺少 #app 挂载节点');
}

const app = createApp(App, {
  initialHostContext: shadowRuntime?.context,
  subscribeHostContext: shadowRuntime?.onContextChange,
  shadowContainer: Boolean(shadowRuntime),
});

app.mount(mountTarget);
shadowRuntime?.registerCleanup(() => app.unmount());
