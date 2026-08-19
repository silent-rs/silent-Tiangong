<script setup lang="ts">
/**
 * 浏览器插件管理界面（shadow 容器，阶段 3 模型驱动）：
 * 标签模型（tabs-model）是唯一状态源——UI 与 Agent 工具壳共用；宿主只
 * 按模型指令调整 webview 实例（显示/隐藏/导航）。本组件负责渲染与把
 * webview 位置持续对齐到内容区（窗口逻辑坐标）。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { createTiangongBridge, getShadowHostRuntime, type HostBridge } from '@tiangong/plugin-sdk';
import { tabsModel, type BrowserTab } from './tabs-model';

const bridge = ref<HostBridge | null>(null);
const ready = ref(false);
const url = ref('');
const tabs = ref<BrowserTab[]>([]);
const activeTabId = ref<string | null>(null);
const status = ref('初始化…');

const contentRef = ref<HTMLElement | null>(null);
let observer: ResizeObserver | null = null;
let syncTimer = 0;

/** 模型状态同步到界面。 */
function modelToUi(): void {
  tabs.value = [...tabsModel.tabs];
  activeTabId.value = tabsModel.activeTabId;
  const active = tabsModel.tabs.find((tab) => tab.id === tabsModel.activeTabId);
  url.value = active?.url ?? '';
}

/** 显示当前活跃标签并对齐内容区矩形。 */
async function syncPosition(): Promise<void> {
  const host = contentRef.value;
  if (!host || !bridge.value || !tabsModel.activeTabId) return;
  const rect = host.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return;
  await tabsModel
    .showTab(tabsModel.activeTabId, {
      x: rect.x,
      y: rect.y,
      width: rect.width,
      height: rect.height,
    })
    .catch(() => {});
  modelToUi();
}

function scheduleSync(): void {
  void syncPosition();
  // 布局晚一拍稳定（标签条增减/窗口拖拽），延迟复测兜底
  window.clearTimeout(syncTimer);
  syncTimer = window.setTimeout(() => void syncPosition(), 120);
}

function normalizeUrl(raw: string): string {
  const value = raw.trim();
  if (!value) return '';
  if (/^[a-z][a-z0-9+.-]*:/i.test(value)) return value;
  return `https://${value}`;
}

async function navigateTo(raw: string): Promise<void> {
  const target = normalizeUrl(raw);
  if (!target) return;
  status.value = '导航中…';
  try {
    // 无标签时导航即建首个标签
    if (tabsModel.tabs.length === 0) {
      await tabsModel.newTab(target);
    } else {
      await tabsModel.navigate(target);
    }
    status.value = '就绪';
    modelToUi();
  } catch (error) {
    status.value = `导航失败：${String(error)}`;
  }
  scheduleSync();
}

async function action(method: 'webview.back' | 'webview.forward' | 'webview.reload'): Promise<void> {
  try {
    await tabsModel.call(method, {});
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function newTab(): Promise<void> {
  try {
    await tabsModel.newTab('about:blank');
    modelToUi();
    status.value = '新标签页';
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function switchTab(tabId: string): Promise<void> {
  if (tabId === tabsModel.activeTabId) return;
  const host = contentRef.value;
  const rect = host?.getBoundingClientRect();
  try {
    if (rect && rect.width > 0) {
      await tabsModel.showTab(tabId, {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
      });
    }
    modelToUi();
  } catch (error) {
    status.value = String(error);
  }
}

async function closeTab(tabId: string): Promise<void> {
  try {
    await tabsModel.closeTab(tabId);
    modelToUi();
    if (tabsModel.tabs.length === 0) status.value = '当前会话还没有页面，点 + 或输入网址开始';
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function hide(): Promise<void> {
  await tabsModel.hideCurrent().catch(() => {});
  status.value = '已隐藏（页面仍在后台）';
}

onMounted(async () => {
  try {
    bridge.value = await createTiangongBridge();
    await tabsModel.attach(bridge.value);
    const runtime = getShadowHostRuntime();
    tabsModel.scope = runtime?.context.session?.id ?? '__global__';
    await tabsModel.restore();
    modelToUi();
    ready.value = true;
    status.value = tabsModel.tabs.length > 0 ? '就绪' : '当前会话还没有页面，点 + 或输入网址开始';
    if (tabsModel.activeTabId) scheduleSync();

    // 会话上下文变化 → 浏览器跟随切换（旧会话隐藏保留，新会话恢复）
    runtime?.onContextChange((context) => {
      const next = context.session?.id ?? '__global__';
      void (async () => {
        status.value = '切换会话…';
        const changed = await tabsModel.switchScope(next);
        if (!changed) return;
        modelToUi();
        status.value =
          tabsModel.tabs.length > 0 ? '就绪' : '当前会话还没有页面，点 + 或输入网址开始';
        if (tabsModel.activeTabId) scheduleSync();
      })();
    });

    // 页面事件通道：标题/地址变化实时回填模型与界面
    bridge.value.on('webview.event', (raw) => {
      try {
        const event = JSON.parse(raw) as {
          event?: string;
          scope?: string;
          payload?: { tab_id?: string; url?: string; title?: string };
        };
        const expected =
          tabsModel.scope === '__global__'
            ? 'webview:browser-handler'
            : `webview:browser-handler:${tabsModel.scope}`;
        if (event.scope !== expected || !event.payload) return;
        if (event.event === 'navigation_failed') {
          status.value = '页面加载失败';
          return;
        }
        if (event.event === 'page_loaded' && event.payload.tab_id) {
          tabsModel.applyPageLoaded(
            event.payload.tab_id,
            event.payload.url,
            event.payload.title,
          );
          modelToUi();
        }
      } catch {
        /* 忽略坏帧 */
      }
    });

    observer = new ResizeObserver(() => scheduleSync());
    if (contentRef.value) observer.observe(contentRef.value);
    window.addEventListener('resize', scheduleSync);
    window.addEventListener('scroll', scheduleSync, true);
  } catch (error) {
    status.value = `初始化失败：${String(error)}`;
  }
});

onBeforeUnmount(() => {
  window.clearTimeout(syncTimer);
  window.removeEventListener('resize', scheduleSync);
  window.removeEventListener('scroll', scheduleSync, true);
  observer?.disconnect();
  observer = null;
  void tabsModel.suspend();
});

// shadow 容器卸载回调（面板关闭/重建）同样隐藏并落盘
getShadowHostRuntime()?.registerCleanup(() => {
  void tabsModel.suspend();
});

function tabLabel(tab: BrowserTab): string {
  if (tab.title) return tab.title;
  try {
    return new URL(tab.url).hostname;
  } catch {
    return tab.url || '新标签页';
  }
}
</script>

<template>
  <div class="browser">
    <!-- 标签条 -->
    <div v-if="tabs.length > 0" class="tabs">
      <div
        v-for="tab in tabs"
        :key="tab.id"
        class="tab"
        :class="{ active: tab.id === activeTabId }"
        @click="switchTab(tab.id)"
      >
        <span class="tab-label">{{ tabLabel(tab) }}</span>
        <span class="tab-close" @click.stop="closeTab(tab.id)">×</span>
      </div>
      <button class="tab-new" title="新建标签" @click="newTab">+</button>
    </div>

    <!-- 工具栏 -->
    <div class="toolbar">
      <button class="nav" title="后退" :disabled="!ready" @click="action('webview.back')">‹</button>
      <button class="nav" title="前进" :disabled="!ready" @click="action('webview.forward')">›</button>
      <button class="nav" title="刷新" :disabled="!ready" @click="action('webview.reload')">⟳</button>
      <input
        v-model="url"
        type="text"
        placeholder="输入网址，回车打开"
        :disabled="!ready"
        @keyup.enter="navigateTo(url)"
      />
      <button class="nav" title="隐藏页面（保留会话）" :disabled="!ready" @click="hide">—</button>
      <span class="status">{{ status }}</span>
    </div>

    <!-- 内容区：原生 webview 叠放在此区域之上，仅作占位与位置锚点 -->
    <div ref="contentRef" class="content">
      <div v-if="!ready || tabs.length === 0" class="placeholder">
        {{ ready ? '当前会话还没有页面，点 + 或输入网址开始' : status }}
      </div>
    </div>
  </div>
</template>

<style scoped>
.browser { display: flex; flex-direction: column; height: 100%; min-height: 0; background: var(--background, #1e1e2e); color: var(--foreground, #cdd6f4); font-size: 12px; }

.tabs { display: flex; align-items: center; gap: 2px; padding: 4px 6px 0; border-bottom: 1px solid var(--border, #8884); min-height: 30px; overflow-x: auto; }
.tab { display: flex; align-items: center; gap: 6px; max-width: 180px; padding: 4px 8px; border: 1px solid var(--border, #8884); border-bottom: 0; border-radius: 6px 6px 0 0; opacity: 0.65; cursor: pointer; white-space: nowrap; }
.tab.active { opacity: 1; background: var(--accent, #8882); }
.tab-label { overflow: hidden; text-overflow: ellipsis; }
.tab-close { padding: 0 2px; border-radius: 3px; }
.tab-close:hover { background: var(--destructive, #f8717188); color: white; }
.tab-new { border: 0; background: transparent; color: inherit; font-size: 14px; cursor: pointer; padding: 2px 8px; }

.toolbar { display: flex; align-items: center; gap: 6px; padding: 6px 8px; border-bottom: 1px solid var(--border, #8884); }
.nav { border: 0; border-radius: 4px; padding: 4px 9px; background: var(--accent, #8883); color: inherit; cursor: pointer; font-size: 13px; }
.nav:disabled { opacity: 0.4; cursor: default; }
.nav:not(:disabled):hover { background: var(--accent-foreground, #8885); }
.toolbar input { flex: 1; min-width: 0; padding: 5px 10px; border: 1px solid var(--border, #8886); border-radius: 999px; background: transparent; color: inherit; font: inherit; }
.status { font-size: 11px; color: var(--muted-foreground, #888); white-space: nowrap; max-width: 160px; overflow: hidden; text-overflow: ellipsis; }

.content { position: relative; flex: 1; min-height: 0; }
.placeholder { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; color: var(--muted-foreground, #888); }
</style>
