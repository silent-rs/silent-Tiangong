/**
 * 浏览器标签模型（阶段 3：标签语义上移插件）：
 * - 本地权威状态：标签列表与活跃标签由插件维护，宿主只持有 webview 实例；
 * - 持久化：插件私有存储（bridge storage.*，按会话作用域分键），应用或
 *   面板重开后按存储重建标签（页面重新加载）；
 * - UI（App.vue）与 Agent 工具壳（shell.ts）共用同一模型，所有标签操作
 *   必经模型，保证状态与存储一致。
 */
import { pluginStorage, type HostBridge } from '@tiangong/plugin-sdk';

export interface BrowserTab {
  id: string;
  url: string;
  title: string;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

const GLOBAL_SCOPE = '__global__';

function storageKey(scope: string): string {
  return `tabs:${scope}`;
}

function newTabId(): string {
  return crypto.randomUUID();
}

export class TabsModel {
  private bridge: HostBridge | null = null;
  /** 当前会话作用域（'__global__' 表示无活跃会话）。 */
  scope = GLOBAL_SCOPE;
  tabs: BrowserTab[] = [];
  activeTabId: string | null = null;

  private persistTimer = 0;

  async attach(bridge: HostBridge): Promise<void> {
    this.bridge = bridge;
  }

  async call<T>(method: string, payload: Record<string, unknown>): Promise<T> {
    const raw = await this.bridge!.call(
      method,
      JSON.stringify({ session_id: this.scope, ...payload }),
    );
    return JSON.parse(raw) as T;
  }

  private async callScope<T>(scope: string, method: string, payload: Record<string, unknown>): Promise<T> {
    const raw = await this.bridge!.call(
      method,
      JSON.stringify({ session_id: scope, ...payload }),
    );
    return JSON.parse(raw) as T;
  }

  /** 恢复标签模型：宿主实例列表为准（真源），插件存储记忆活跃偏好与
   * 标题补充；宿主自身也持久化标签（双保险，应用重启后同样对齐）。 */
  async restore(): Promise<void> {
    const saved = await this.readSaved();
    const snapshot = await this.call<{ tabs?: BrowserTab[]; active_tab_id?: string | null }>(
      'webview.tabs',
      {},
    );
    const hostTabs = snapshot.tabs ?? [];
    this.tabs = hostTabs.map((tab) => {
      const remembered = saved?.tabs?.find((item) => item.id === tab.id);
      return {
        id: tab.id,
        url: tab.url,
        title: tab.title || remembered?.title || '',
      };
    });
    const rememberedActive = saved?.active_tab_id;
    this.activeTabId =
      (rememberedActive && this.tabs.some((tab) => tab.id === rememberedActive)
        ? rememberedActive
        : null) ??
      snapshot.active_tab_id ??
      this.tabs[0]?.id ??
      null;
  }

  private async readSaved(): Promise<{
    tabs?: BrowserTab[];
    active_tab_id?: string | null;
  } | null> {
    const raw = await pluginStorage.get(this.bridge!, storageKey(this.scope)).catch(() => null);
    if (!raw) return null;
    try {
      return JSON.parse(raw);
    } catch {
      return null;
    }
  }

  /** 持久化（500ms 合并防抖）。 */
  schedulePersist(): void {
    window.clearTimeout(this.persistTimer);
    this.persistTimer = window.setTimeout(() => void this.persist(), 500);
  }

  private async persist(): Promise<void> {
    if (!this.bridge) return;
    const payload = JSON.stringify({ tabs: this.tabs, active_tab_id: this.activeTabId });
    await pluginStorage.set(this.bridge!, storageKey(this.scope), payload).catch(() => {});
  }

  /** 新建标签（本地生成编号，宿主按编号建 webview）。 */
  async newTab(url: string): Promise<BrowserTab | null> {
    const id = newTabId();
    await this.call('webview.tabNew', { url, tab_id: id });
    const tab: BrowserTab = { id, url, title: '' };
    this.tabs.push(tab);
    this.activeTabId = id;
    this.schedulePersist();
    return tab;
  }

  /** 关闭标签；关闭的是活跃标签时活跃转移到相邻标签。 */
  async closeTab(tabId: string): Promise<void> {
    await this.call('webview.tabClose', { tab_id: tabId });
    const index = this.tabs.findIndex((tab) => tab.id === tabId);
    if (index >= 0) this.tabs.splice(index, 1);
    if (this.activeTabId === tabId) {
      this.activeTabId = this.tabs[Math.min(index, this.tabs.length - 1)]?.id ?? null;
    }
    this.schedulePersist();
  }

  /** 显示指定标签到给定矩形（宿主按指令调整 webview；显示语义互斥）。 */
  async showTab(tabId: string, rect: Rect): Promise<void> {
    await this.call('webview.instanceShow', { tab_id: tabId, ...rect });
    this.activeTabId = tabId;
    this.schedulePersist();
  }

  /** 导航当前活跃标签。 */
  async navigate(url: string): Promise<void> {
    await this.call('webview.navigate', { url });
    const active = this.tabs.find((tab) => tab.id === this.activeTabId);
    if (active) active.url = url;
    this.schedulePersist();
  }

  /** 页面事件回填（标题/最终地址）。 */
  applyPageLoaded(tabId: string, url?: string, title?: string): void {
    const tab = this.tabs.find((item) => item.id === tabId);
    if (!tab) return;
    if (url) tab.url = url;
    if (title) tab.title = title;
    this.schedulePersist();
  }

  /** 隐藏当前作用域的所有页面（切换会话/面板关闭时；实例保留）。 */
  async hideCurrent(): Promise<void> {
    if (!this.bridge) return;
    await this.callScope(this.scope, 'webview.hide', {}).catch(() => {});
  }

  /** 切换会话作用域：隐藏旧会话页面（实例保留），恢复新会话模型。 */
  async switchScope(next: string): Promise<boolean> {
    if (!this.bridge || next === this.scope) return false;
    const previous = this.scope;
    await this.persist().catch(() => {});
    await this.callScope(previous, 'webview.hide', {}).catch(() => {});
    this.scope = next;
    await this.restore();
    return true;
  }

  /** 面板卸载：隐藏当前会话页面并落盘模型。 */
  async suspend(): Promise<void> {
    window.clearTimeout(this.persistTimer);
    await this.persist().catch(() => {});
    await this.hideCurrent();
  }
}

/** 插件级共享单例：UI 与工具壳共用同一标签模型。 */
export const tabsModel = new TabsModel();
