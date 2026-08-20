<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import {
  ArrowLeft,
  ArrowRight,
  Clock,
  CornerDownRight,
  Globe,
  History,
  LoaderCircle,
  PenTool,
  Plus,
  RotateCw,
  ScanSearch,
  Trash2,
  X,
  ZoomIn,
  ZoomOut,
} from 'lucide-vue-next';
import {
  createTiangongBridge,
  getShadowHostRuntime,
  type HostBridge,
  type HostContext,
} from '@tiangong/plugin-sdk';

interface BrowserTab {
  id: string;
  url: string;
  title: string;
}

interface HistoryEntry {
  url: string;
  title: string;
  timestamp: number;
}

interface TabHistory {
  tab_id: string;
  entries: HistoryEntry[];
  current_index: number;
}

interface ExtractedElement {
  tag: string;
  text: string;
  selector: string;
  attributes: Record<string, string>;
}

interface WebviewEvent {
  event?: 'navigation_started' | 'navigation_failed' | 'page_loaded';
  scope?: string;
  payload?: {
    tab_id?: string;
    url?: string;
    title?: string;
    navigation_id?: number;
  };
}

const DEFAULT_URL = 'about:blank';
const GLOBAL_SCOPE = '__global__';
const HISTORY_PAGE_SIZE = 20;
const MIN_ZOOM = 0.25;
const MAX_ZOOM = 5;

const bridge = ref<HostBridge | null>(null);
const ready = ref(false);
const url = ref('');
const page = ref<BrowserTab | null>(null);
const instanceId = ref<string | null>(null);
const tabHistory = ref<TabHistory>({ tab_id: '', entries: [], current_index: -1 });
const zoom = ref(1);
const annotationActive = ref(false);
const extractedElements = ref<ExtractedElement[] | null>(null);
const isLoading = ref(false);
const notice = ref<{ kind: 'error' | 'info'; text: string } | null>(null);

const showHistoryModal = ref(false);
const globalHistoryEntries = ref<HistoryEntry[]>([]);
const globalHistoryOffset = ref(0);
const globalHistoryHasMore = ref(true);
const globalHistoryLoading = ref(false);
const historyCloseRef = ref<HTMLButtonElement | null>(null);

const contentRef = ref<HTMLElement | null>(null);
let observer: ResizeObserver | null = null;
let syncTimer = 0;
let noticeTimer = 0;
let cleaned = false;
let panelVisible = false;
let currentScope = GLOBAL_SCOPE;
let contextTicket = 0;
const cleanups: Array<() => void> = [];

const hasPage = computed(() => Boolean(page.value && !isBlankBrowserUrl(page.value.url)));
const canGoBack = computed(() => tabHistory.value.current_index > 0);
const canGoForward = computed(() => (
  tabHistory.value.current_index >= 0
  && tabHistory.value.current_index < tabHistory.value.entries.length - 1
));

function isBlankBrowserUrl(value: string): boolean {
  return !value || value === DEFAULT_URL;
}

function displayUrl(value: string): string {
  return isBlankBrowserUrl(value) ? '' : value;
}

function normalizeBrowserUrl(raw: string): string {
  const value = raw.trim();
  if (!value) return '';
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//i.test(value)) return value;
  if (/^about:/i.test(value)) return value;
  if (/^\//.test(value)) return `file://${value}`;
  return `https://${value}`;
}

function formatTime(timestamp: number): string {
  const date = new Date(timestamp);
  const diffMinutes = Math.floor((Date.now() - date.getTime()) / 60_000);
  if (diffMinutes < 1) return '刚刚';
  if (diffMinutes < 60) return `${diffMinutes} 分钟前`;
  const diffHours = Math.floor(diffMinutes / 60);
  if (diffHours < 24) return `${diffHours} 小时前`;
  const diffDays = Math.floor(diffHours / 24);
  if (diffDays < 7) return `${diffDays} 天前`;
  return date.toLocaleDateString();
}

function showNotice(text: string, kind: 'error' | 'info' = 'error'): void {
  window.clearTimeout(noticeTimer);
  notice.value = { kind, text };
  noticeTimer = window.setTimeout(() => {
    notice.value = null;
  }, 5_000);
}

interface WebviewSnapshot {
  tabs?: BrowserTab[];
  active_tab_id?: string | null;
}

async function callWebview<T>(
  method: string,
  payload: Record<string, unknown> = {},
  scope = currentScope,
): Promise<T> {
  if (!bridge.value) throw new Error('浏览器桥接尚未就绪');
  const raw = await bridge.value.call(
    method,
    JSON.stringify({ session_id: scope, ...payload }),
  );
  return JSON.parse(raw) as T;
}

function applySnapshot(snapshot: WebviewSnapshot): void {
  const current = snapshot.tabs?.find((tab) => tab.id === instanceId.value) ?? null;
  page.value = current ? { ...current } : null;
  url.value = displayUrl(current?.url ?? '');
}

async function refreshPage(): Promise<void> {
  applySnapshot(await callWebview<WebviewSnapshot>('webview.tabs'));
}

async function refreshTabHistory(tabId = instanceId.value): Promise<void> {
  if (!tabId) {
    tabHistory.value = { tab_id: '', entries: [], current_index: -1 };
    return;
  }
  try {
    const result = await callWebview<TabHistory>('webview.tabHistory', { tab_id: tabId });
    if (tabId === instanceId.value) tabHistory.value = result;
  } catch {
    if (tabId === instanceId.value) {
      tabHistory.value = { tab_id: tabId, entries: [], current_index: -1 };
    }
  }
}

async function refreshZoom(): Promise<void> {
  try {
    const result = await callWebview<{ scale?: number }>('webview.getZoom');
    zoom.value = typeof result.scale === 'number' ? result.scale : 1;
  } catch {
    zoom.value = 1;
  }
}

async function hideInstance(
  tabId = instanceId.value,
  scope = currentScope,
): Promise<void> {
  if (!tabId || !bridge.value) return;
  await callWebview('webview.instanceHide', { tab_id: tabId }, scope).catch(() => {});
}

async function showInstance(): Promise<boolean> {
  const host = contentRef.value;
  const tabId = instanceId.value;
  if (!host || !bridge.value || !tabId || !panelVisible || showHistoryModal.value) return false;
  const rect = host.getBoundingClientRect();
  if (rect.width <= 0 || rect.height <= 0) return false;
  await callWebview('webview.instanceShow', {
    tab_id: tabId,
    x: rect.x,
    y: rect.y,
    width: rect.width,
    height: rect.height,
  });
  return true;
}

async function syncPosition(): Promise<void> {
  await showInstance().catch(() => {});
}

function scheduleSync(): void {
  void syncPosition();
  window.clearTimeout(syncTimer);
  syncTimer = window.setTimeout(() => void syncPosition(), 120);
}

async function navigateTo(raw: string): Promise<void> {
  const target = normalizeBrowserUrl(raw);
  if (!target || !ready.value) return;
  isLoading.value = true;
  notice.value = null;
  try {
    if (!await showInstance()) {
      isLoading.value = false;
      return;
    }
    const snapshot = await callWebview<WebviewSnapshot>('webview.navigate', { url: target });
    applySnapshot(snapshot);
    if (isBlankBrowserUrl(target)) isLoading.value = false;
    scheduleSync();
  } catch (error) {
    isLoading.value = false;
    showNotice(`无法打开页面：${String(error)}`);
  }
}

function requestNewTab(): void {
  if (!ready.value) return;
  window.dispatchEvent(new CustomEvent('tiangong:plugin-request-new', {
    detail: { plugin_id: 'browser', contribution_id: 'browser' },
  }));
}

async function stopAnnotation(): Promise<void> {
  if (!annotationActive.value) return;
  const tabId = instanceId.value;
  if (!tabId) return;
  await callWebview('webview.instanceEval', {
    tab_id: tabId,
    js: 'window.__tiangong_bridge?.annotation?.stop()',
  }).catch(() => {});
  annotationActive.value = false;
}

async function navigationAction(method: 'webview.back' | 'webview.forward' | 'webview.reload'): Promise<void> {
  if (!ready.value || !instanceId.value) return;
  isLoading.value = true;
  try {
    if (!await showInstance()) {
      isLoading.value = false;
      return;
    }
    await callWebview(method);
    scheduleSync();
  } catch (error) {
    isLoading.value = false;
    showNotice(`页面操作失败：${String(error)}`);
  }
}

async function setZoom(scale: number): Promise<void> {
  await stopAnnotation();
  try {
    if (!await showInstance()) return;
    const result = await callWebview<{ scale?: number }>('webview.setZoom', { scale });
    zoom.value = result.scale ?? scale;
  } catch (error) {
    showNotice(`调整缩放失败：${String(error)}`);
  }
}

async function resetZoom(): Promise<void> {
  await stopAnnotation();
  try {
    if (!await showInstance()) return;
    const result = await callWebview<{ scale?: number }>('webview.resetZoom');
    zoom.value = result.scale ?? 1;
  } catch (error) {
    showNotice(`重置缩放失败：${String(error)}`);
  }
}

async function toggleAnnotation(): Promise<void> {
  if (!hasPage.value) return;
  try {
    if (annotationActive.value) {
      await stopAnnotation();
    } else {
      if (!await showInstance()) return;
      await callWebview('webview.eval', {
        js: 'window.__tiangong_bridge.annotation.start("rect")',
      });
      annotationActive.value = true;
    }
  } catch (error) {
    showNotice(`批注操作失败：${String(error)}`);
  }
}

async function extractAnnotations(): Promise<void> {
  try {
    if (!await showInstance()) return;
    const result = await callWebview<{
      elements?: Array<{ elements?: ExtractedElement[] }>;
    }>('webview.annotationExtract');
    const elements = (result.elements ?? []).flatMap((entry) => entry.elements ?? []);
    extractedElements.value = elements;
    if (elements.length === 0) showNotice('没有提取到框选元素', 'info');
  } catch (error) {
    showNotice(`提取批注失败：${String(error)}`);
  }
}

async function loadGlobalHistory(offset: number): Promise<void> {
  if (globalHistoryLoading.value) return;
  globalHistoryLoading.value = true;
  try {
    const entries = await callWebview<HistoryEntry[]>('webview.globalHistory', {
      offset,
      limit: HISTORY_PAGE_SIZE,
    });
    globalHistoryEntries.value = offset === 0
      ? entries
      : [...globalHistoryEntries.value, ...entries];
    globalHistoryHasMore.value = entries.length >= HISTORY_PAGE_SIZE;
    globalHistoryOffset.value = offset + entries.length;
  } catch (error) {
    globalHistoryHasMore.value = false;
    showNotice(`加载历史失败：${String(error)}`);
  } finally {
    globalHistoryLoading.value = false;
  }
}

async function openHistoryModal(): Promise<void> {
  if (!ready.value) return;
  await hideInstance();
  globalHistoryEntries.value = [];
  globalHistoryOffset.value = 0;
  globalHistoryHasMore.value = true;
  showHistoryModal.value = true;
  void loadGlobalHistory(0);
  await nextTick();
  historyCloseRef.value?.focus();
}

function closeHistoryModal(): void {
  if (!showHistoryModal.value) return;
  showHistoryModal.value = false;
  scheduleSync();
}

async function jumpToHistory(targetUrl: string): Promise<void> {
  showHistoryModal.value = false;
  url.value = targetUrl;
  await navigateTo(targetUrl);
}

async function deleteHistoryEntry(targetUrl: string, event: Event): Promise<void> {
  event.stopPropagation();
  try {
    await callWebview('webview.globalHistoryDelete', { url: targetUrl });
    globalHistoryEntries.value = globalHistoryEntries.value.filter((entry) => entry.url !== targetUrl);
  } catch (error) {
    showNotice(`删除历史失败：${String(error)}`);
  }
}

async function clearGlobalHistory(): Promise<void> {
  try {
    await callWebview('webview.globalHistoryClear');
    globalHistoryEntries.value = [];
    globalHistoryOffset.value = 0;
    globalHistoryHasMore.value = false;
  } catch (error) {
    showNotice(`清空历史失败：${String(error)}`);
  }
}

function handleHistoryScroll(event: Event): void {
  const element = event.currentTarget as HTMLElement;
  if (
    element.scrollHeight - element.scrollTop - element.clientHeight < 100
    && globalHistoryHasMore.value
    && !globalHistoryLoading.value
  ) {
    void loadGlobalHistory(globalHistoryOffset.value);
  }
}

function expectedScope(): string {
  return `webview:browser:${currentScope}`;
}

async function handleWebviewEvent(raw: string): Promise<void> {
  let event: WebviewEvent;
  try {
    event = JSON.parse(raw) as WebviewEvent;
  } catch {
    return;
  }
  if (event.scope !== expectedScope() || !event.payload) return;

  const { tab_id: tabId, url: eventUrl, title } = event.payload;
  if (!tabId || tabId !== instanceId.value) return;
  if (event.event === 'navigation_started') {
    if (eventUrl) {
      page.value = { id: tabId, url: eventUrl, title: page.value?.title ?? '' };
      url.value = displayUrl(eventUrl);
    }
    annotationActive.value = false;
    extractedElements.value = null;
    isLoading.value = true;
    return;
  }

  if (event.event === 'navigation_failed') {
    isLoading.value = false;
    showNotice('页面加载失败');
    await refreshTabHistory(tabId);
    return;
  }

  if (event.event === 'page_loaded') {
    page.value = {
      id: tabId,
      url: eventUrl ?? page.value?.url ?? DEFAULT_URL,
      title: title || page.value?.title || '',
    };
    url.value = displayUrl(page.value.url);
    isLoading.value = false;
    await refreshTabHistory(tabId);
    scheduleSync();
  }
}

function handleKeyDown(event: KeyboardEvent): void {
  if (!panelVisible) return;
  if (event.key === 'Escape' && showHistoryModal.value) {
    event.preventDefault();
    closeHistoryModal();
    return;
  }
  if (!(event.metaKey || event.ctrlKey) || showHistoryModal.value) return;
  if (event.key === '=' || event.key === '+') {
    event.preventDefault();
    if (zoom.value < MAX_ZOOM) void setZoom(+(zoom.value + 0.1).toFixed(2));
  } else if (event.key === '-') {
    event.preventDefault();
    if (zoom.value > MIN_ZOOM) void setZoom(+(zoom.value - 0.1).toFixed(2));
  } else if (event.key === '0') {
    event.preventDefault();
    void resetZoom();
  }
}

async function applyHostContext(context: HostContext): Promise<void> {
  const ticket = ++contextTicket;
  const previousScope = currentScope;
  const previousInstanceId = instanceId.value;
  const nextScope = context.session?.id ?? GLOBAL_SCOPE;
  const nextInstanceId = context.app?.instance_id ?? null;
  const nextVisible = context.app?.visible === true;

  panelVisible = false;
  if (
    previousInstanceId
    && (
      previousScope !== nextScope
      || previousInstanceId !== nextInstanceId
      || !nextVisible
    )
  ) {
    await hideInstance(previousInstanceId, previousScope);
  }
  if (cleaned || ticket !== contextTicket) return;

  currentScope = nextScope;
  instanceId.value = nextInstanceId;
  panelVisible = Boolean(nextInstanceId && nextVisible);
  showHistoryModal.value = false;
  annotationActive.value = false;
  extractedElements.value = null;
  isLoading.value = false;

  if (!nextInstanceId) {
    page.value = null;
    url.value = '';
    ready.value = false;
    return;
  }
  if (!panelVisible) {
    ready.value = true;
    return;
  }

  try {
    await refreshPage();
    if (cleaned || ticket !== contextTicket) return;
    ready.value = true;
    await showInstance();
    await Promise.all([refreshZoom(), refreshTabHistory(nextInstanceId)]);
    scheduleSync();
  } catch (error) {
    if (ticket === contextTicket) showNotice(`浏览器初始化失败：${String(error)}`);
  }
}

function cleanup(): void {
  if (cleaned) return;
  cleaned = true;
  window.clearTimeout(syncTimer);
  window.clearTimeout(noticeTimer);
  observer?.disconnect();
  observer = null;
  cleanups.splice(0).reverse().forEach((stop) => stop());
  window.removeEventListener('resize', scheduleSync);
  window.removeEventListener('scroll', scheduleSync, true);
  window.removeEventListener('keydown', handleKeyDown, true);
  panelVisible = false;
  void hideInstance();
}

onMounted(async () => {
  try {
    bridge.value = await createTiangongBridge();
    const runtime = getShadowHostRuntime();

    cleanups.push(bridge.value.on('webview.event', (raw) => {
      void handleWebviewEvent(raw);
    }));
    if (runtime) {
      cleanups.push(runtime.onContextChange((context) => void applyHostContext(context)));
      runtime.registerCleanup(cleanup);
    } else {
      currentScope = GLOBAL_SCOPE;
      const snapshot = await callWebview<WebviewSnapshot>('webview.tabs');
      instanceId.value = snapshot.active_tab_id ?? snapshot.tabs?.[0]?.id ?? null;
      panelVisible = Boolean(instanceId.value);
      applySnapshot(snapshot);
      ready.value = Boolean(instanceId.value);
      if (ready.value) {
        await showInstance();
        await Promise.all([refreshZoom(), refreshTabHistory()]);
        scheduleSync();
      }
    }

    observer = new ResizeObserver(scheduleSync);
    if (contentRef.value) observer.observe(contentRef.value);
    window.addEventListener('resize', scheduleSync);
    window.addEventListener('scroll', scheduleSync, true);
    window.addEventListener('keydown', handleKeyDown, true);
  } catch (error) {
    showNotice(`浏览器初始化失败：${String(error)}`);
  }
});

onBeforeUnmount(cleanup);
</script>

<template>
  <div class="browser-shell">
    <div class="toolbar">
      <div class="toolbar-controls">
        <button type="button" class="icon-button" title="新建标签" aria-label="新建标签" :disabled="!ready" @click="requestNewTab"><Plus /></button>
        <span class="toolbar-divider" />
        <button type="button" class="icon-button" title="后退" aria-label="后退" :disabled="!ready || !canGoBack" @click="navigationAction('webview.back')"><ArrowLeft /></button>
        <button type="button" class="icon-button" title="前进" aria-label="前进" :disabled="!ready || !canGoForward" @click="navigationAction('webview.forward')"><ArrowRight /></button>
        <button type="button" class="icon-button" title="刷新" aria-label="刷新" :disabled="!ready || !hasPage" @click="navigationAction('webview.reload')">
          <LoaderCircle v-if="isLoading" class="spin" />
          <RotateCw v-else />
        </button>
        <span class="toolbar-divider" />
        <button type="button" class="icon-button" title="缩小 (Cmd/Ctrl -)" aria-label="缩小" :disabled="!ready || zoom <= MIN_ZOOM + 1e-6" @click="setZoom(+(zoom - 0.1).toFixed(2))"><ZoomOut /></button>
        <button type="button" class="zoom-value" title="双击重置为 100% (Cmd/Ctrl 0)" @dblclick="resetZoom">{{ Math.round(zoom * 100) }}%</button>
        <button type="button" class="icon-button" title="放大 (Cmd/Ctrl +)" aria-label="放大" :disabled="!ready || zoom >= MAX_ZOOM - 1e-6" @click="setZoom(+(zoom + 0.1).toFixed(2))"><ZoomIn /></button>
        <button type="button" class="icon-button" title="浏览历史" aria-label="浏览历史" :disabled="!ready" @click="openHistoryModal"><History /></button>
      </div>

      <div class="address-group">
        <Globe class="address-icon" aria-hidden="true" />
        <input v-model="url" type="text" inputmode="url" autocomplete="off" spellcheck="false" placeholder="输入 URL..." aria-label="页面地址" :disabled="!ready" @keydown.enter.prevent="navigateTo(url)" />
        <button type="button" class="go-button" title="进入" :disabled="!ready || !url.trim()" @click="navigateTo(url)"><CornerDownRight /><span>进入</span></button>
        <button type="button" class="icon-button" :class="{ selected: annotationActive }" :title="annotationActive ? '关闭批注' : '开启批注'" :aria-label="annotationActive ? '关闭批注' : '开启批注'" :aria-pressed="annotationActive" :disabled="!ready || !hasPage" @click="toggleAnnotation"><PenTool /></button>
        <button v-if="annotationActive" type="button" class="icon-button" title="提取框选元素" aria-label="提取框选元素" @click="extractAnnotations"><ScanSearch /></button>
      </div>
    </div>

    <div v-if="notice" class="notice" :class="notice.kind" role="status">
      <span>{{ notice.text }}</span>
      <button type="button" title="关闭" aria-label="关闭提示" @click="notice = null"><X /></button>
    </div>

    <div v-if="extractedElements && extractedElements.length > 0" class="annotation-results">
      <div class="annotation-header">
        <span>提取到 {{ extractedElements.length }} 个元素</span>
        <button type="button" title="关闭" aria-label="关闭提取结果" @click="extractedElements = null"><X /></button>
      </div>
      <div v-for="(element, index) in extractedElements" :key="`${element.selector}-${index}`" class="annotation-row">
        <span class="element-tag">&lt;{{ element.tag }}&gt;</span>
        <span v-if="element.text" class="element-text">{{ element.text }}</span>
        <div class="element-selector">{{ element.selector }}</div>
      </div>
    </div>

    <div ref="contentRef" class="content-anchor" />

    <div v-if="showHistoryModal" class="modal-backdrop" role="presentation" @mousedown.self="closeHistoryModal">
      <section class="history-dialog" role="dialog" aria-modal="true" aria-labelledby="history-title">
        <header class="dialog-header">
          <h2 id="history-title"><Clock />浏览历史</h2>
          <button ref="historyCloseRef" type="button" class="icon-button" title="关闭" aria-label="关闭浏览历史" @click="closeHistoryModal"><X /></button>
        </header>
        <div class="history-list" @scroll="handleHistoryScroll">
          <div v-if="globalHistoryEntries.length === 0 && !globalHistoryLoading" class="history-empty">暂无浏览历史</div>
          <div v-for="(entry, index) in globalHistoryEntries" :key="`${entry.url}-${entry.timestamp}-${index}`" role="button" tabindex="0" class="history-entry" @click="jumpToHistory(entry.url)" @keydown.enter.prevent="jumpToHistory(entry.url)" @keydown.space.prevent="jumpToHistory(entry.url)">
            <span class="history-copy">
              <strong>{{ entry.title || entry.url }}</strong>
              <span class="history-meta"><span>{{ entry.url.replace(/^https?:\/\//, '').split('/')[0] }}</span><span>{{ formatTime(entry.timestamp) }}</span></span>
            </span>
            <button type="button" class="history-delete" title="删除" aria-label="删除历史记录" @click="deleteHistoryEntry(entry.url, $event)"><X /></button>
          </div>
          <div v-if="globalHistoryLoading" class="history-loading"><LoaderCircle class="spin" /><span>加载中...</span></div>
        </div>
        <footer v-if="globalHistoryEntries.length > 0" class="dialog-footer">
          <button type="button" class="clear-history" @click="clearGlobalHistory"><Trash2 /><span>清空全部历史</span></button>
        </footer>
      </section>
    </div>
  </div>
</template>

<style scoped>
:global(#app) {
  height: 100%;
  min-height: 0;
}

:host,
*,
*::before,
*::after {
  box-sizing: border-box;
  letter-spacing: 0;
}

button,
input {
  font: inherit;
  letter-spacing: 0;
}

button { color: inherit; }

.browser-shell {
  display: flex;
  width: 100%;
  height: 100%;
  min-width: 0;
  min-height: 0;
  flex-direction: column;
  overflow: hidden;
  background: hsl(var(--background, 0 0% 100%));
  color: hsl(var(--foreground, 222.2 47.4% 11.2%));
  font-family: inherit;
  font-size: 12px;
}

.history-delete {
  display: inline-flex;
  width: 18px;
  height: 18px;
  flex: 0 0 18px;
  align-items: center;
  justify-content: center;
  border: 0;
  border-radius: 3px;
  padding: 0;
  background: transparent;
  color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
  cursor: pointer;
}

.history-delete:hover { background: hsl(var(--destructive, 0 84.2% 60.2%) / 0.12); color: hsl(var(--destructive, 0 84.2% 60.2%)); }
.history-delete svg { width: 12px; height: 12px; }

.toolbar {
  display: flex;
  min-width: 0;
  flex: 0 0 auto;
  align-items: center;
  gap: 6px;
  border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  padding: 7px 8px;
}

.toolbar-controls,
.address-group { display: flex; min-width: 0; align-items: center; gap: 4px; }
.toolbar-controls { flex: 0 0 auto; }
.address-group { flex: 1 1 320px; }
.toolbar-divider { width: 1px; height: 18px; margin: 0 2px; background: hsl(var(--border, 214.3 31.8% 91.4%)); }

.icon-button,
.zoom-value,
.go-button,
.annotation-header button,
.notice button {
  display: inline-flex;
  height: 28px;
  flex: 0 0 auto;
  align-items: center;
  justify-content: center;
  border: 0;
  border-radius: 4px;
  background: transparent;
  cursor: pointer;
}

.icon-button { width: 28px; padding: 0; }
.icon-button svg,
.go-button svg,
.annotation-header svg,
.notice svg,
.dialog-header svg,
.clear-history svg,
.history-loading svg { width: 14px; height: 14px; stroke-width: 1.8; }
.icon-button:hover:not(:disabled),
.zoom-value:hover:not(:disabled) { background: hsl(var(--muted, 210 40% 96.1%)); }
.icon-button.selected { background: hsl(var(--primary, 222.2 47.4% 11.2%)); color: hsl(var(--primary-foreground, 210 40% 98%)); }
.icon-button:disabled,
.go-button:disabled { cursor: default; opacity: 0.38; }
.zoom-value { width: 42px; padding: 0 4px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); font-variant-numeric: tabular-nums; }

.address-icon { width: 16px; height: 16px; flex: 0 0 16px; margin-left: 2px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); stroke-width: 1.8; }
.address-group input { width: 100%; min-width: 120px; height: 28px; flex: 1 1 auto; border: 1px solid hsl(var(--input, 214.3 31.8% 91.4%)); border-radius: 6px; outline: none; padding: 0 9px; background: transparent; color: hsl(var(--foreground, 222.2 47.4% 11.2%)); font-size: 13px; }
.address-group input::placeholder { color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.address-group input:focus { border-color: hsl(var(--ring, 222.2 47.4% 11.2%)); box-shadow: 0 0 0 1px hsl(var(--ring, 222.2 47.4% 11.2%) / 0.35); }
.go-button { gap: 4px; padding: 0 8px; }
.go-button:hover:not(:disabled) { background: hsl(var(--muted, 210 40% 96.1%)); }

.notice { display: flex; min-height: 30px; flex: 0 0 auto; align-items: center; justify-content: space-between; gap: 8px; border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%)); padding: 5px 10px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.notice.error { background: hsl(var(--destructive, 0 84.2% 60.2%) / 0.08); color: hsl(var(--destructive, 0 84.2% 60.2%)); }
.notice span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.notice button,
.annotation-header button { width: 24px; height: 24px; padding: 0; }

.annotation-results { max-height: 160px; flex: 0 0 auto; overflow-y: auto; border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%)); padding: 7px 8px; background: hsl(var(--muted, 210 40% 96.1%) / 0.2); }
.annotation-header { display: flex; align-items: center; justify-content: space-between; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); font-weight: 500; }
.annotation-row { min-width: 0; border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%) / 0.3); padding: 5px 0; }
.annotation-row:last-child { border-bottom: 0; }
.element-tag { color: hsl(var(--primary, 222.2 47.4% 11.2%)); font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
.element-text { display: inline-block; max-width: 60%; margin-left: 5px; overflow: hidden; text-overflow: ellipsis; vertical-align: bottom; white-space: nowrap; }
.element-selector { overflow: hidden; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }

.content-anchor { position: relative; min-width: 0; min-height: 0; flex: 1 1 auto; background: hsl(var(--muted, 210 40% 96.1%) / 0.3); }
.modal-backdrop { position: fixed; z-index: 1000; inset: 0; display: flex; align-items: center; justify-content: center; padding: 16px; background: rgb(0 0 0 / 0.48); }
.history-dialog { display: flex; width: min(560px, 100%); max-height: min(640px, calc(100vh - 32px)); flex-direction: column; overflow: hidden; border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%)); border-radius: 8px; background: hsl(var(--popover, 0 0% 100%)); color: hsl(var(--popover-foreground, 222.2 47.4% 11.2%)); box-shadow: 0 18px 48px rgb(0 0 0 / 0.28); }
.dialog-header { display: flex; min-height: 52px; flex: 0 0 auto; align-items: center; justify-content: space-between; border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%)); padding: 10px 14px; }
.dialog-header h2 { display: flex; align-items: center; gap: 8px; margin: 0; font-size: 16px; font-weight: 600; }
.history-list { min-height: 120px; flex: 1 1 auto; overflow-y: auto; padding: 0 14px; }
.history-empty { padding: 48px 12px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); text-align: center; }
.history-entry { display: flex; width: 100%; min-width: 0; align-items: flex-start; gap: 12px; border: 0; border-bottom: 1px solid hsl(var(--border, 214.3 31.8% 91.4%) / 0.55); border-radius: 4px; padding: 10px 8px; background: transparent; cursor: pointer; text-align: left; }
.history-entry:hover { background: hsl(var(--muted, 210 40% 96.1%) / 0.55); }
.history-copy { display: block; min-width: 0; flex: 1; }
.history-copy strong { display: block; overflow: hidden; font-size: 14px; font-weight: 500; text-overflow: ellipsis; white-space: nowrap; }
.history-meta { display: flex; min-width: 0; gap: 8px; margin-top: 3px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.history-meta span:first-child { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.history-delete { opacity: 0; }
.history-entry:hover .history-delete,
.history-delete:focus-visible { opacity: 1; }
.history-loading { display: flex; align-items: center; justify-content: center; gap: 7px; padding: 16px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.dialog-footer { flex: 0 0 auto; border-top: 1px solid hsl(var(--border, 214.3 31.8% 91.4%)); padding: 12px 14px; }
.clear-history { display: inline-flex; width: 100%; height: 32px; align-items: center; justify-content: center; gap: 6px; border: 0; border-radius: 4px; background: hsl(var(--destructive, 0 84.2% 60.2%)); color: hsl(var(--destructive-foreground, 210 40% 98%)); cursor: pointer; }
.clear-history:hover { opacity: 0.9; }

.spin { animation: spin 0.9s linear infinite; }
button:focus-visible,
input:focus-visible,
[role='button']:focus-visible { outline: 2px solid hsl(var(--ring, 222.2 47.4% 11.2%) / 0.65); outline-offset: 1px; }
@keyframes spin { to { transform: rotate(360deg); } }

@media (max-width: 760px) {
  .toolbar { flex-wrap: wrap; gap: 5px; }
  .toolbar-controls { width: 100%; overflow-x: auto; scrollbar-width: none; }
  .toolbar-controls::-webkit-scrollbar { display: none; }
  .address-group { width: 100%; flex-basis: 100%; }
  .go-button span { display: none; }
  .go-button { width: 28px; padding: 0; }
}

@media (hover: none) {
  .history-delete { opacity: 1; }
}
</style>
