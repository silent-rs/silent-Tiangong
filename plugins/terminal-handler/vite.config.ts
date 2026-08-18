import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { viteSingleFile } from 'vite-plugin-singlefile';

// 单文件产物：宿主 iframe 容器以 srcdoc 注入，JS/CSS 必须内联自包含。
// base './' 保证资源相对引用（插件目录内按相对路径取回）。
export default defineConfig({
  base: './',
  plugins: [vue(), viteSingleFile()],
});
