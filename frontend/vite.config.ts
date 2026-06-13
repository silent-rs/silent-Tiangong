/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  test: {
    environment: 'jsdom',
  },
  // Tauri 需要使用 1337 端口或自定义端口
  server: {
    port: 5173,
    strictPort: true,
    // 绑定所有接口（IPv4 + IPv6）：node 18+ 默认只绑 ::1，Tauri 用 127.0.0.1 探测
    // 时会一直 Waiting for your frontend dev server。host:true 同时绑 0.0.0.0 和 [::]。
    host: true,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ['**/src-tauri/**'],
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks: {
          // React 核心库
          'react-core': ['react', 'react-dom', 'react-dom/client'],
          // UI 组件库
          'ui-components': ['lucide-react'],
          // Markdown 渲染
          'markdown': ['md-editor-rt'],
          // Tauri API
          'tauri': ['@tauri-apps/api/core', '@tauri-apps/api/event'],
          // 工具库
          'utils': ['zustand', 'clsx', 'tailwind-merge'],
        },
      },
    },
    chunkSizeWarningLimit: 600, // 警告阈值提高到 600KB
  },
})
