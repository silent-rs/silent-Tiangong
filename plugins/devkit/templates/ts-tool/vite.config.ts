import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { viteSingleFile } from 'vite-plugin-singlefile';

// 单文件产物：宿主 Shadow 容器动态注入，iframe 兼容模式也可用 srcdoc 加载。
// base './' 保证资源相对引用（插件目录内按相对路径取回）。
export default defineConfig({
  base: './',
  plugins: [vue(), viteSingleFile()],
});
