import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { viteSingleFile } from 'vite-plugin-singlefile';

// 单文件产物：宿主 iframe 容器以 srcdoc 注入，JS/CSS 必须内联自包含。
// base './' 保证资源相对引用（插件目录内按相对路径取回）。
// shadow 容器以经典脚本方式执行（new Function，bridge 参数注入），
// 关闭 modulepreload 避免产物携带 import.meta（仅 module 环境合法）。
export default defineConfig({
  base: './',
  plugins: [vue(), viteSingleFile()],
  build: {
    modulePreload: false,
  },
});
