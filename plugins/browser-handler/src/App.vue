<script setup lang="ts">
/**
 * 浏览器插件管理界面（shadow 容器）：
 * webview 页面本体由宿主容器原语渲染（sandbox: webview），
 * 本界面承载地址栏/状态（导航操作经 bridge webview.*）。
 */
import { ref } from 'vue';

const url = ref('');
const status = ref('就绪');

async function navigate() {
  if (!url.value.trim()) return;
  status.value = '导航中…';
  try {
    // 经宿主 webview 容器原语导航（工具同通道）
    const bridge = (window as unknown as { __tiangong_bridge?: { call: (m: string, p: string) => Promise<string> } }).__tiangong_bridge;
    if (bridge) {
      await bridge.call('webview.navigate', JSON.stringify({ url: url.value }));
      status.value = '已导航';
    } else {
      status.value = '桥接未就绪';
    }
  } catch (error) {
    status.value = `失败：${String(error)}`;
  }
}
</script>

<template>
  <div class="toolbar">
    <input v-model="url" type="text" placeholder="输入网址…" @keyup.enter="navigate" />
    <button @click="navigate">前往</button>
    <span class="status">{{ status }}</span>
  </div>
</template>

<style scoped>
.toolbar { display: flex; gap: 6px; padding: 6px; border-bottom: 1px solid var(--border, #8884); }
input[type='text'] { flex: 1; padding: 5px 8px; border: 1px solid var(--border, #8886); border-radius: 4px; background: transparent; color: inherit; font: inherit; font-size: 12px; }
button { border: 0; border-radius: 4px; padding: 5px 10px; background: var(--primary, #2563eb); color: white; cursor: pointer; font-size: 12px; }
.status { font-size: 11px; color: var(--muted-foreground, #888); align-self: center; }
</style>
