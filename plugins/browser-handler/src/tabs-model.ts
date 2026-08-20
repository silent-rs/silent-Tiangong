/**
 * 浏览器标签模型（阶段 3：标签语义上移插件）：
 * - 宿主快照是真源，插件维护对应的界面状态与活跃偏好；
 * - 持久化：宿主持久化真实标签，插件私有存储按会话记录界面偏好；
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

export class TabsModel {
  private bridge: HostBridge | null = null;
  private readonly listeners = new Set<() => void>();
  /** 当前会话作用域（'__global__' 表示无活跃会话）。 */
  scope = GLOBAL_SCOPE;
  tabs: BrowserTab[] = [];
  activeTabId: string | null = null;

  private persistTimer = 0;

  async attach(bridge: HostBridge): Promise<void> {
    this.bridge = bridge;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private notify(): void {
    this.listeners.forEach((listener) => listener());
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

  private applySnapshot(
    snapshot: { tabs?: BrowserTab[]; active_tab_id?: string | null },
    saved?: { tabs?: BrowserTab[]; active_tab_id?: string | null } | null,
  ): void {
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
    this.notify();
  }

  /** 恢复标签模型：宿主实例列表为准（真源），插件存储只补充活跃偏好。 */
  async restore(): Promise<void> {
    const [saved, snapshot] = await Promise.all([
      this.readSaved(),
      this.call<{ tabs?: BrowserTab[]; active_tab_id?: string | null }>('webview.tabs', {}),
    ]);
    this.applySnapshot(snapshot, saved);
  }

  /** 从宿主刷新标签快照，供页面事件和 Agent 操作后同步界面。 */
  async refresh(): Promise<void> {
    const snapshot = await this.call<{ tabs?: BrowserTab[]; active_tab_id?: string | null }>(
      'webview.tabs',
      {},
    );
    this.applySnapshot(snapshot);
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

  /** 新建标签；编号由宿主按项目统一的 SCRU128 规则生成。 */
  async newTab(url: string): Promise<BrowserTab | null> {
    const snapshot = await this.call<{
      tab_id?: string;
      tabs?: BrowserTab[];
      active_tab_id?: string | null;
    }>('webview.tabNew', { url });
    this.applySnapshot(snapshot);
    this.schedulePersist();
    return this.tabs.find((tab) => tab.id === snapshot.tab_id) ?? null;
  }

  /** 关闭标签；关闭的是活跃标签时活跃转移到相邻标签。 */
  async closeTab(tabId: string): Promise<void> {
    const snapshot = await this.call<{ tabs?: BrowserTab[]; active_tab_id?: string | null }>(
      'webview.tabClose',
      { tab_id: tabId },
    );
    this.applySnapshot(snapshot);
    this.schedulePersist();
  }

  /** 显示指定标签到给定矩形（宿主按指令调整 webview；显示语义互斥）。 */
  async showTab(tabId: string, rect: Rect): Promise<void> {
    await this.call('webview.instanceShow', { tab_id: tabId, ...rect });
    const changed = this.activeTabId !== tabId;
    this.activeTabId = tabId;
    if (changed) this.notify();
    this.schedulePersist();
  }

  /** 导航当前活跃标签。 */
  async navigate(url: string): Promise<void> {
    const snapshot = await this.call<{ tabs?: BrowserTab[]; active_tab_id?: string | null }>(
      'webview.navigate',
      { url },
    );
    this.applySnapshot(snapshot);
    this.schedulePersist();
  }

  /** 页面事件回填（标题/最终地址）。 */
  applyPageLoaded(tabId: string, url?: string, title?: string): void {
    const tab = this.tabs.find((item) => item.id === tabId);
    if (!tab) return;
    if (url) tab.url = url;
    if (title) tab.title = title;
    this.notify();
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
