import { createApp } from 'vue';
import { getShadowHostRuntime } from '@tiangong/plugin-sdk';
import App from './App.vue';

const shadowRuntime = getShadowHostRuntime();
const mountTarget = (shadowRuntime?.root ?? document).querySelector('#app');

if (!(mountTarget instanceof HTMLElement)) {
  throw new Error('交互处理器缺少 #app 挂载节点');
}

Object.assign(mountTarget.style, {
  boxSizing: 'border-box',
  width: '100%',
  height: '100%',
  maxHeight: 'inherit',
  margin: '0',
  overflow: 'hidden',
  background: 'transparent',
});

if (!shadowRuntime) {
  Object.assign(document.documentElement.style, {
    width: '100%',
    height: '100%',
    margin: '0',
    background: 'transparent',
  });
  Object.assign(document.body.style, {
    width: '100%',
    height: '100%',
    margin: '0',
    overflow: 'hidden',
    background: 'transparent',
  });
}

const app = createApp(App, {
  initialHostContext: shadowRuntime?.context,
  subscribeHostContext: shadowRuntime?.onContextChange,
  shadowContainer: Boolean(shadowRuntime),
});

app.mount(mountTarget);
shadowRuntime?.registerCleanup(() => app.unmount());
