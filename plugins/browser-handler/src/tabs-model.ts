/**
 * Agent 工具壳使用的宿主页面快照。页面与活跃状态以宿主 webview 原语为准，
 * 可见标签、切换、关闭和持久化统一由 App 拓展区顶部标签处理。
 */
import type { HostBridge } from '@tiangong/plugin-sdk';

export interface BrowserTab {
  id: string;
  url: string;
  title: string;
}

interface BrowserSnapshot {
  tab_id?: string;
  tabs?: BrowserTab[];
  active_tab_id?: string | null;
}

const GLOBAL_SCOPE = '__global__';

export class TabsModel {
  private bridge: HostBridge | null = null;
  /** 当前会话作用域（'__global__' 表示无活跃会话）。 */
  scope = GLOBAL_SCOPE;
  tabs: BrowserTab[] = [];
  activeTabId: string | null = null;

  async attach(bridge: HostBridge): Promise<void> {
    this.bridge = bridge;
  }

  async call<T>(method: string, payload: Record<string, unknown>): Promise<T> {
    if (!this.bridge) throw new Error('浏览器桥接尚未就绪');
    const raw = await this.bridge.call(
      method,
      JSON.stringify({ session_id: this.scope, ...payload }),
    );
    return JSON.parse(raw) as T;
  }

  private applySnapshot(snapshot: BrowserSnapshot): void {
    this.tabs = (snapshot.tabs ?? []).map((tab) => ({ ...tab }));
    this.activeTabId = snapshot.active_tab_id ?? this.tabs[0]?.id ?? null;
  }

  /** 读取当前会话的宿主页面快照。 */
  async restore(): Promise<void> {
    this.applySnapshot(await this.call<BrowserSnapshot>('webview.tabs', {}));
  }

  /** 新建页面；编号由宿主按项目统一的 SCRU128 规则生成。 */
  async newTab(url: string): Promise<BrowserTab | null> {
    const snapshot = await this.call<BrowserSnapshot>('webview.tabNew', { url });
    this.applySnapshot(snapshot);
    return this.tabs.find((tab) => tab.id === snapshot.tab_id) ?? null;
  }

  /** 导航宿主当前活跃页面并刷新快照。 */
  async navigate(url: string): Promise<void> {
    this.applySnapshot(await this.call<BrowserSnapshot>('webview.navigate', { url }));
  }
}

/** 单个工具壳实例内共享的宿主页面快照。 */
export const tabsModel = new TabsModel();
