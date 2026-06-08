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
