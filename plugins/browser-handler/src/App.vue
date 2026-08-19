<script setup lang="ts">
/**
 * 浏览器插件管理界面（shadow 容器）：
 * 页面本体是宿主原生 webview 实例（经 webview.* 容器原语管理），
 * 本界面承载地址栏/导航/标签条，并把 webview 位置持续对齐到内容区
 * （窗口逻辑坐标，与内置浏览器同一同步通道）。
 *
 * 会话隔离（对齐终端插件）：每个对话一套独立的多标签浏览器，切换
 * 对话自动跟随——旧会话页面隐藏（实例保留），新会话按需恢复/新建。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { createTiangongBridge, getShadowHostRuntime, type HostBridge } from '@tiangong/plugin-sdk';

interface TabInfo {
  id: string;
  url: string;
  title: string;
}

/** 无活跃会话时的全局作用域。 */
const GLOBAL_SCOPE = '__global__';
/** 当前跟随的会话作用域。 */
const scope = ref(GLOBAL_SCOPE);

const bridge = ref<HostBridge | null>(null);
const ready = ref(false);
const url = ref('');
const tabs = ref<TabInfo[]>([]);
const activeTabId = ref<string | null>(null);
const status = ref('初始化…');

const contentRef = ref<HTMLElement | null>(null);
let observer: ResizeObserver | null = null;
let syncTimer = 0;
/** 会话切换防抖序号：异步切换中会话再变时丢弃过期结果。 */
let switchTicket = 0;

async function call<T>(
  method: string,
  payload: Record<string, unknown>,
  scopeOverride?: string,
): Promise<T> {
  const raw = await bridge.value!.call(
    method,
    JSON.stringify({ session_id: scopeOverride ?? scope.value, ...payload }),
  );
  return JSON.parse(raw) as T;
}

/** 把原生 webview 显示并对齐到内容区矩形（窗口逻辑坐标）。 */
async function syncPosition(): Promise<void> {
  const host = contentRef.value;
  if (!host || !bridge.value) return;
  const rect = host.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return;
  await call('webview.show', {
    x: rect.x,
    y: rect.y,
    width: rect.width,
    height: rect.height,
  }).catch(() => {});
}

async function refreshTabs(): Promise<void> {
  const snapshot = await call<{ tabs?: TabInfo[]; active_tab_id?: string | null }>('webview.tabs', {});
  tabs.value = snapshot.tabs ?? [];
  activeTabId.value = snapshot.active_tab_id ?? null;
  const active = tabs.value.find((tab) => tab.id === activeTabId.value);
  url.value = active?.url ?? '';
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
    await call('webview.navigate', { url: target });
    await refreshTabs();
    status.value = '已导航';
  } catch (error) {
    status.value = `导航失败：${String(error)}`;
  }
  scheduleSync();
}

async function action(method: string): Promise<void> {
  try {
    await call(method, {});
    await refreshTabs();
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function newTab(): Promise<void> {
  try {
    // 无实例时 create（含默认标签）；有实例则新增标签
    const method = tabs.value.length === 0 ? 'webview.create' : 'webview.tabNew';
    const snapshot = await call<{ tabs?: TabInfo[]; active_tab_id?: string | null }>(method, {
      url: 'about:blank',
    });
    tabs.value = snapshot.tabs ?? [];
    activeTabId.value = snapshot.active_tab_id ?? null;
    url.value = '';
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function switchTab(tabId: string): Promise<void> {
  if (tabId === activeTabId.value) return;
  try {
    const snapshot = await call<{ tabs?: TabInfo[]; active_tab_id?: string | null }>(
      'webview.tabSwitch',
      { tab_id: tabId },
    );
    tabs.value = snapshot.tabs ?? [];
    activeTabId.value = snapshot.active_tab_id ?? null;
    const active = tabs.value.find((tab) => tab.id === activeTabId.value);
    url.value = active?.url ?? '';
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function closeTab(tabId: string): Promise<void> {
  try {
    const snapshot = await call<{ tabs?: TabInfo[]; active_tab_id?: string | null }>(
      'webview.tabClose',
      { tab_id: tabId },
    );
    tabs.value = snapshot.tabs ?? [];
    activeTabId.value = snapshot.active_tab_id ?? null;
  } catch (error) {
    status.value = String(error);
  }
  scheduleSync();
}

async function hide(): Promise<void> {
  await call('webview.hide', {}).catch(() => {});
  status.value = '已隐藏（页面仍在后台）';
}

/** 跟随会话切换标签集：旧会话页面隐藏（实例保留），新会话恢复/空态。 */
async function switchScope(next: string): Promise<void> {
  if (!bridge.value || next === scope.value) return;
  const ticket = ++switchTicket;
  const previous = scope.value;
  scope.value = next;
  tabs.value = [];
  activeTabId.value = null;
  url.value = '';
  status.value = '切换会话…';
  // 旧会话页面隐藏（webview 实例与标签保留，切回即恢复）
  if (ready.value) {
    await call('webview.hide', {}, previous).catch(() => {});
  }
  try {
    const existing = await call<{ tabs?: TabInfo[] }>('webview.tabs', {});
    if (ticket !== switchTicket) return;
    tabs.value = existing.tabs ?? [];
    ready.value = true;
    if (tabs.value.length > 0) {
      await refreshTabs();
      status.value = '就绪';
      scheduleSync();
    } else {
      status.value = '当前会话还没有页面，点 + 或输入网址开始';
    }
  } catch (error) {
    status.value = `切换失败：${String(error)}`;
  }
}

onMounted(async () => {
  try {
    bridge.value = await createTiangongBridge();
    // 初始作用域：当前对话（无活跃会话时全局共享）
    const runtime = getShadowHostRuntime();
    const initial = runtime?.context.session?.id ?? GLOBAL_SCOPE;
    scope.value = initial;
    // 已有会话则复用（create 会把现有标签导航走，仅在无实例时创建）；
    // 无标签保持空态，等用户或 Agent 打开第一个页面
    const existing = await call<{ tabs?: TabInfo[] }>('webview.tabs', {});
    if (existing.tabs?.length) {
      await refreshTabs();
      status.value = '就绪';
    } else {
      status.value = '当前会话还没有页面，点 + 或输入网址开始';
    }
    ready.value = true;
    scheduleSync();

    // 会话上下文变化 → 浏览器跟随切换（对齐终端插件行为）
    runtime?.onContextChange((context) => {
      const next = context.session?.id ?? GLOBAL_SCOPE;
      void switchScope(next);
    });

    // 页面事件通道（宿主定向推送）：加载完成/失败时实时刷新标签状态，
    // 不再依赖操作后主动查询（SPA 内跳转、页面标题变化都能跟上）。
    let refreshTimer = 0;
    bridge.value.on('webview.event', (raw) => {
      try {
        const event = JSON.parse(raw) as {
          event?: string;
          scope?: string;
          payload?: { tab_id?: string; url?: string; title?: string };
        };
        // 仅响应当前会话作用域的事件
        const expected = scope.value === GLOBAL_SCOPE
          ? 'webview:browser-handler'
          : `webview:browser-handler:${scope.value}`;
        if (event.scope !== expected || !event.payload) return;
        if (event.event === 'navigation_failed') {
          status.value = '页面加载失败';
          return;
        }
        if (event.event === 'page_loaded') {
          // 增量更新当前标签标题/地址，合并短时多次事件
          const active = tabs.value.find((tab) => tab.id === event.payload?.tab_id);
          if (active && event.payload) {
            if (event.payload.title) active.title = event.payload.title;
            if (event.payload.url) active.url = event.payload.url;
            if (active.id === activeTabId.value) url.value = active.url;
          }
          window.clearTimeout(refreshTimer);
          refreshTimer = window.setTimeout(() => void refreshTabs().catch(() => {}), 200);
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
  // 容器卸载时隐藏当前会话页面（webview 进程与标签保留，切回恢复）
  void bridge.value
    ?.call('webview.hide', JSON.stringify({ session_id: scope.value }))
    .catch(() => {});
});

// shadow 容器卸载回调（面板关闭/重建）同样隐藏当前会话页面
getShadowHostRuntime()?.registerCleanup(() => {
  void bridge.value
    ?.call('webview.hide', JSON.stringify({ session_id: scope.value }))
    .catch(() => {});
});

function tabLabel(tab: TabInfo): string {
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
      <div v-if="!ready" class="placeholder">{{ status }}</div>
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
