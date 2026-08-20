import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Globe, Grid3x3, Puzzle, X } from 'lucide-react';
import { api } from '@/api/tauri';
import type { SandboxKind, TabKind, TabState } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { AgentTeamPanel } from './AgentTeamPanel';
import { PluginAppTabContent } from './PluginAppTabContent';
import { runPluginBeforeClose } from './PluginSandbox';
import { Button } from './ui/button';
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from './ui/context-menu';

interface TabsContainerProps {
  initialTabKind: TabKind;
  isVisible: boolean;
  openRequestVersion: number;
  onClose: () => void;
  /** 点击启动台按钮：拓展区切回 App 矩阵态（面板保持展开）。 */
  onShowMatrix?: () => void;
  /** tab 集合（按类型）变化通知：宿主「已打开」绿点的即时数据源，
   *  覆盖新建/关闭/会话恢复，且不受持久化时序影响（新对话未落盘也生效）。 */
  onTabKindsChanged?: (
    kinds: TabKind[],
    pluginApps: string[],
    pluginInstances: PluginAppInstanceRef[],
  ) => void;
  /** 宿主下发的 App 实例命令（矩阵右键菜单：新建实例/关闭全部实例）。 */
  appCommand?: AppTabCommand | null;
  /** 拓展区模式：app（聚焦实例）或 matrix（App 矩阵占据内容区，tab 栏保留）。 */
  mode?: 'app' | 'matrix';
  /** 矩阵态渲染到内容区的视图（由宿主注入，保持容器与矩阵解耦）。 */
  matrix?: ReactNode;
  onActiveKindChange?: (kind: TabKind | null) => void;
}

const DEFAULT_BROWSER_URL = 'about:blank';
const TABS_PERSIST_DEBOUNCE_MS = 500;

/** 宿主（矩阵菜单等）下发的 App 实例命令，version 递增触发执行。 */
export interface AppTabCommand {
  kind: TabKind;
  action: 'close-all' | 'close-plugin' | 'open-plugin';
  version: number;
  /** 命令所属会话；会话标签完成恢复且仍为当前会话时才执行。 */
  sessionId?: string;
  /** open-plugin 携带的三方 App 元数据；close-plugin 仅使用 pluginId。 */
  app?: {
    pluginId: string;
    contributionId: string;
    title: string;
    sandbox: SandboxKind;
    multi: boolean;
    /** 已由调用方创建或导航到的 webview 页面编号。 */
    instanceId?: string;
    /** 工具调用拉起等宿主侧场景置 true：已有实例时聚焦而非新建（multi 亦然）。 */
    focusExisting?: boolean;
  };
}

/** 已接入拓展区顶部标签的通用 App 实例引用。 */
export interface PluginAppInstanceRef {
  pluginId: string;
  contributionId: string;
  instanceId: string;
  sessionId: string;
}

interface WebviewPluginTabInfo {
  id: string;
  url: string;
  title: string;
}

interface WebviewPluginSnapshot {
  tab_id?: string;
  tabs?: WebviewPluginTabInfo[];
  active_tab_id?: string | null;
}

function isWebviewPluginTab(tab: TabState): boolean {
  return tab.kind === 'plugin'
    && tab.sandbox === 'webview'
    && Boolean(tab.plugin_id && tab.contribution_id);
}

function webviewSessionId(sessionId: string): string {
  return sessionId || '__global__';
}

async function callWebviewPlugin<T>(
  pluginId: string,
  sessionId: string,
  method: string,
  payload: Record<string, unknown> = {},
): Promise<T> {
  const raw = await api.bridgeCall(
    pluginId,
    method,
    JSON.stringify({ session_id: webviewSessionId(sessionId), ...payload }),
  );
  return JSON.parse(raw) as T;
}

function createWebviewPluginTab(
  page: WebviewPluginTabInfo,
  app: NonNullable<AppTabCommand['app']>,
): TabState {
  return {
    id: page.id,
    kind: 'plugin',
    title: page.title || app.title,
    url: page.url || DEFAULT_BROWSER_URL,
    created_at: nowText(),
    plugin_id: app.pluginId,
    contribution_id: app.contributionId,
    sandbox: app.sandbox,
  };
}

async function reconcileWebviewPluginTabs(
  sessionId: string,
  sourceTabs: TabState[],
  sourceActiveTabId: string | null,
): Promise<{ tabs: TabState[]; activeTabId: string | null }> {
  let tabs = [...sourceTabs];
  let activeTabId = sourceActiveTabId;
  const groups = new Map<string, TabState[]>();
  for (const tab of sourceTabs.filter(isWebviewPluginTab)) {
    const key = `${tab.plugin_id}:${tab.contribution_id}`;
    groups.set(key, [...(groups.get(key) ?? []), tab]);
  }

  for (const group of groups.values()) {
    const sample = group[0];
    if (!sample?.plugin_id || !sample.contribution_id) continue;
    try {
      const snapshot = await callWebviewPlugin<WebviewPluginSnapshot>(
        sample.plugin_id,
        sessionId,
        'webview.tabs',
      );
      const app: NonNullable<AppTabCommand['app']> = {
        pluginId: sample.plugin_id,
        contributionId: sample.contribution_id,
        title: sample.plugin_id === 'browser' ? '浏览器' : sample.title,
        sandbox: 'webview',
        multi: true,
      };
      const runtimeTabs = (snapshot.tabs ?? []).map((page) => createWebviewPluginTab(page, app));
      const groupIds = new Set(group.map((tab) => tab.id));
      const insertAt = Math.max(0, tabs.findIndex((tab) => groupIds.has(tab.id)));
      tabs = tabs.filter((tab) => !groupIds.has(tab.id));
      tabs.splice(Math.min(insertAt, tabs.length), 0, ...runtimeTabs);
      if (activeTabId && groupIds.has(activeTabId)) {
        activeTabId = snapshot.active_tab_id
          ?? runtimeTabs[0]?.id
          ?? tabs[0]?.id
          ?? null;
      }
    } catch (error) {
      console.warn('恢复 webview 插件顶部标签失败：', error);
    }
  }

  return { tabs, activeTabId };
}

function nowText(): string {
  return new Date().toISOString();
}

function normalizeActiveTabId(tabs: TabState[], activeTabId: string | null | undefined): string {
  if (activeTabId && tabs.some((tab) => tab.id === activeTabId)) {
    return activeTabId;
  }
  return tabs[0]?.id ?? '';
}

function pickNextActiveTab(tabs: TabState[], closedIndex: number): string | null {
  if (tabs.length === 0) return null;
  const nextIndex = Math.min(closedIndex, tabs.length - 1);
  return tabs[nextIndex]?.id ?? tabs[0]?.id ?? null;
}

export function TabsContainer({
  initialTabKind,
  isVisible,
  openRequestVersion,
  onClose,
  onShowMatrix,
  onTabKindsChanged,
  appCommand,
  mode = 'app',
  matrix,
  onActiveKindChange,
}: TabsContainerProps) {
  const activeSessionId = useStore((state) => state.activeSessionId);
  const newConversationId = useStore((state) => state.newConversationId);
  const [tabs, setTabs] = useState<TabState[]>([]);
  const [activeTabId, setActiveTabId] = useState('');
  const [hydrateVersion, setHydrateVersion] = useState(0);
  const [activationRetryVersion, setActivationRetryVersion] = useState(0);
  const tabsRef = useRef<TabState[]>([]);
  const activeTabIdRef = useRef('');
  const activeSessionIdRef = useRef<string | null>(null);
  const initialTabKindRef = useRef<TabKind>(initialTabKind);
  const isVisibleRef = useRef(isVisible);
  const lastInitialActivationKeyRef = useRef<string | null>(null);
  const hydratingSessionRef = useRef<string | null>(null);
  const hydratedSessionRef = useRef<string | null>(null);
  const persistTimerRef = useRef<number | null>(null);
  const terminalSessionIdRef = useRef('');
  const terminalSessionId = activeSessionId || newConversationId || '';
  terminalSessionIdRef.current = terminalSessionId;

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null,
    [activeTabId, tabs],
  );

  const openWebviewPluginTab = useCallback(async (
    app: NonNullable<AppTabCommand['app']>,
    requestedInstanceId?: string,
  ) => {
    if (app.sandbox !== 'webview') return;
    try {
      let snapshot = await callWebviewPlugin<WebviewPluginSnapshot>(
        app.pluginId,
        terminalSessionId,
        'webview.tabs',
      );
      // 已有页面（如 Agent 后台操作中的标签）时直接聚焦（协同观察）；
      // 仅在无页面或矩阵「新建实例」（multi 且非聚焦语义）时新建空白页。
      if (
        !requestedInstanceId
        && ((snapshot.tabs ?? []).length === 0 || (app.multi && !app.focusExisting))
      ) {
        snapshot = await callWebviewPlugin<WebviewPluginSnapshot>(
          app.pluginId,
          terminalSessionId,
          'webview.tabNew',
          { url: DEFAULT_BROWSER_URL },
        );
      }

      const instanceId = requestedInstanceId
        ?? snapshot.tab_id
        ?? snapshot.active_tab_id
        ?? snapshot.tabs?.[0]?.id;
      const page = snapshot.tabs?.find((tab) => tab.id === instanceId);
      if (!instanceId || !page) throw new Error('宿主未返回可用的浏览器页面');

      const nextTab = createWebviewPluginTab(page, app);
      const currentTabs = tabsRef.current;
      const nextTabs = currentTabs.some((tab) => tab.id === nextTab.id)
        ? currentTabs.map((tab) => tab.id === nextTab.id ? { ...tab, ...nextTab } : tab)
        : [...currentTabs, nextTab];
      tabsRef.current = nextTabs;
      activeTabIdRef.current = nextTab.id;
      setTabs(nextTabs);
      setActiveTabId(nextTab.id);
    } catch (error) {
      console.error('打开 webview 插件标签失败：', error);
    }
  }, [terminalSessionId]);

  const hideWebviewPluginTabs = useCallback((
    targetTabs: TabState[],
    sessionId = terminalSessionId,
  ) => {
    const plugins = new Set(
      targetTabs
        .filter(isWebviewPluginTab)
        .map((tab) => tab.plugin_id)
        .filter((pluginId): pluginId is string => Boolean(pluginId)),
    );
    for (const pluginId of plugins) {
      void callWebviewPlugin(pluginId, sessionId, 'webview.hide').catch(console.error);
    }
  }, [terminalSessionId]);

  // tab 类型/插件 App 集合变化 → 通知宿主更新「已打开」绿点（即时、无持久化时序依赖）
  const onTabKindsChangedRef = useRef(onTabKindsChanged);
  onTabKindsChangedRef.current = onTabKindsChanged;
  const lastTabKindsRef = useRef<string>('');
  useEffect(() => {
    const kinds = Array.from(new Set(tabs.map((tab) => tab.kind)));
    const pluginInstances = tabs.flatMap((tab): PluginAppInstanceRef[] => (
      tab.kind === 'plugin' && tab.plugin_id && tab.contribution_id
        ? [{
          pluginId: tab.plugin_id,
          contributionId: tab.contribution_id,
          instanceId: tab.id,
          sessionId: terminalSessionId,
        }]
        : []
    ));
    const pluginApps = Array.from(new Set(
      pluginInstances.map((instance) => `${instance.pluginId}:${instance.contributionId}`),
    ));
    const instanceKey = pluginInstances
      .map((instance) => `${instance.pluginId}:${instance.contributionId}:${instance.instanceId}`)
      .join(',');
    const key = `${terminalSessionId}|${kinds.join(',')}|${pluginApps.join(',')}|${instanceKey}`;
    if (key === lastTabKindsRef.current) return;
    lastTabKindsRef.current = key;
    onTabKindsChangedRef.current?.(kinds, pluginApps, pluginInstances);
  }, [tabs, terminalSessionId]);

  useEffect(() => {
    onActiveKindChange?.(isVisible ? activeTab?.kind ?? null : null);
  }, [activeTab?.kind, isVisible, onActiveKindChange]);

  initialTabKindRef.current = initialTabKind;
  isVisibleRef.current = isVisible;

  const restoreRuntimeForTabs = useCallback(async (
    sessionId: string,
    nextTabs: TabState[],
    nextActiveTabId: string,
    visible: boolean,
  ) => {
    if (!visible) {
      hideWebviewPluginTabs(nextTabs, sessionId);
      return;
    }

    const activeTab = nextTabs.find((tab) => tab.id === nextActiveTabId);
    if (!activeTab || !isWebviewPluginTab(activeTab)) {
      hideWebviewPluginTabs(nextTabs, sessionId);
    }
  }, [hideWebviewPluginTabs]);

  useEffect(() => {
    if (isVisible && mode === 'app') return;
    hideWebviewPluginTabs(tabsRef.current);
  }, [hideWebviewPluginTabs, isVisible, mode]);

  useEffect(() => {
    activeTabIdRef.current = activeTabId;
  }, [activeTabId]);

  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);


  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    void api.onBridgeEvent((event) => {
      if (cancelled || event.channel !== 'webview.event') return;
      let pageEvent: {
        scope?: string;
        payload?: { tab_id?: string; url?: string; title?: string };
      };
      try {
        pageEvent = JSON.parse(event.payload);
      } catch {
        return;
      }
      const expectedScope = `webview:${event.plugin_id}:${webviewSessionId(terminalSessionId)}`;
      const tabId = pageEvent.payload?.tab_id;
      if (pageEvent.scope !== expectedScope || !tabId) return;

      let changed = false;
      const nextTabs = tabsRef.current.map((tab) => {
        if (!isWebviewPluginTab(tab) || tab.plugin_id !== event.plugin_id || tab.id !== tabId) {
          return tab;
        }
        changed = true;
        return {
          ...tab,
          url: pageEvent.payload?.url ?? tab.url,
          title: pageEvent.payload?.title || tab.title,
        };
      });
      if (changed) {
        tabsRef.current = nextTabs;
        setTabs(nextTabs);
      }
    }).then((cleanup) => {
      if (cancelled) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [terminalSessionId]);

  const persistTabsNow = useCallback(() => {
    if (persistTimerRef.current !== null) {
      window.clearTimeout(persistTimerRef.current);
      persistTimerRef.current = null;
    }

    const sessionId = activeSessionIdRef.current;
    const currentTabs = tabsRef.current;
    if (hydratingSessionRef.current !== null || !sessionId || currentTabs.length === 0) return;
    if (hydratedSessionRef.current !== sessionId) return;

    const activeId = activeTabIdRef.current || currentTabs[0]?.id || null;
    void api.setSessionTabs(sessionId, currentTabs, activeId).catch(console.error);
  }, []);

  useEffect(() => {
    if (!activeSessionId) {
      if (hydratedSessionRef.current !== newConversationId) {
        hydratedSessionRef.current = newConversationId;
        tabsRef.current = [];
        activeTabIdRef.current = '';
        setTabs([]);
        setActiveTabId('');
      }
      return;
    }

    // 新对话预留 ID 与 Core 创建出的 Session ID 相同，已有本地 Tab 可直接落盘。
    if (hydratedSessionRef.current === activeSessionId) {
      if (tabsRef.current.length > 0) {
        const activeId = activeTabIdRef.current || tabsRef.current[0]?.id || null;
        void api.setSessionTabs(activeSessionId, tabsRef.current, activeId).catch(console.error);
      }
      return;
    }

    if (hydratedSessionRef.current === activeSessionId) return;
    if (hydratingSessionRef.current === activeSessionId) return;

    const sessionId = activeSessionId;
    hydratingSessionRef.current = sessionId;

    let cancelled = false;
    const hydrate = async () => {
      try {
        const sessionTabs = await api.getSessionTabs(sessionId);
        if (cancelled) return;
        // 丢弃旧内置终端/浏览器时代的 tab（无对应运行时，也不再渲染）。
        const pluginTabs = sessionTabs.tabs.filter((tab) => tab.kind === 'plugin');
        const reconciled = await reconcileWebviewPluginTabs(
          sessionId,
          pluginTabs,
          sessionTabs.active_tab_id && pluginTabs.some((tab) => tab.id === sessionTabs.active_tab_id)
            ? sessionTabs.active_tab_id
            : null,
        );
        if (cancelled) return;
        const nextTabs = reconciled.tabs;
        const nextActiveTabId = normalizeActiveTabId(nextTabs, reconciled.activeTabId);

        tabsRef.current = nextTabs;
        activeTabIdRef.current = nextActiveTabId;
        setTabs(nextTabs);
        setActiveTabId(nextActiveTabId);

        // await runtime 恢复完成后再标记已 hydrate（避免 activateOrCreateTab 竞态）
        await restoreRuntimeForTabs(sessionId, nextTabs, nextActiveTabId, isVisibleRef.current);
        if (cancelled) return;

        // 只有完整成功后才标记已恢复
        hydratingSessionRef.current = null;
        hydratedSessionRef.current = sessionId;
        setHydrateVersion((version) => version + 1);
        setActivationRetryVersion((version) => version + 1);
      } catch (err) {
        if (cancelled) return;
        console.error('恢复会话 Tab 失败：', err);
        tabsRef.current = [];
        activeTabIdRef.current = '';
        setTabs([]);
        setActiveTabId('');
        // 持久化读取失败时以空标签进入可用态，不能让 App 打开命令永久
        // 等待；后续新建标签仍会按正常路径重新落盘。
        if (hydratingSessionRef.current === sessionId) {
          hydratingSessionRef.current = null;
        }
        hydratedSessionRef.current = sessionId;
        setHydrateVersion((version) => version + 1);
        setActivationRetryVersion((version) => version + 1);
      }
    };

    void hydrate();
    return () => {
      cancelled = true;
      // cleanup：本次未完成，清除 hydrating 允许重试
      if (hydratingSessionRef.current === sessionId) {
        hydratingSessionRef.current = null;
      }
    };
  }, [activeSessionId, newConversationId, restoreRuntimeForTabs]);

  const activateOrCreateTab = useCallback(async (kind: TabKind) => {
    const sessionId = terminalSessionId;
    if (!sessionId) return;
    // 所有 App（浏览器/终端/三方）均以插件形态存在：创建/聚焦统一走宿主
    // 命令通道（open-plugin），此处仅聚焦已有实例。
    if (kind === 'plugin') {
      const existing = tabsRef.current.find((tab) => tab.kind === 'plugin');
      if (existing) {
        activeTabIdRef.current = existing.id;
        setActiveTabId(existing.id);
      }
      return;
    }
  }, [terminalSessionId]);

  useEffect(() => {
    if (!isVisible) return;
    // 矩阵态 = 用户尚未选择 App：不按 initialTabKind 隐式创建 tab
    // （浏览器/终端已插件化，宿主不再默认开原生 App；显式打开走
    // openWorkspacePanel，会切到 App 态并推进 openRequestVersion 再触发本 effect）。
    if (mode === 'matrix') return;
    if (hydratingSessionRef.current !== null) {
      return;
    }
    const activationKey = `${terminalSessionId}:${hydrateVersion}:${initialTabKind}:${openRequestVersion}`;
    if (lastInitialActivationKeyRef.current === activationKey) {
      return;
    }
    lastInitialActivationKeyRef.current = activationKey;
    void activateOrCreateTab(initialTabKind);
  }, [
    activateOrCreateTab,
    activationRetryVersion,
    hydrateVersion,
    initialTabKind,
    isVisible,
    mode,
    openRequestVersion,
    terminalSessionId,
  ]);

  const handleSwitchTab = useCallback((tabId: string) => {
    const nextTab = tabs.find((tab) => tab.id === tabId);
    const currentTab = tabsRef.current.find((tab) => tab.id === activeTabIdRef.current);
    if (currentTab && currentTab.id !== tabId && isWebviewPluginTab(currentTab) && currentTab.plugin_id) {
      const switchingWithinPlugin = Boolean(
        nextTab
        && isWebviewPluginTab(nextTab)
        && nextTab.plugin_id === currentTab.plugin_id,
      );
      void callWebviewPlugin(
        currentTab.plugin_id,
        terminalSessionId,
        switchingWithinPlugin ? 'webview.instanceHide' : 'webview.hide',
        switchingWithinPlugin ? { tab_id: currentTab.id } : {},
      ).catch(console.error);
    }
    activeTabIdRef.current = tabId;
    setActiveTabId(tabId);
  }, [tabs, terminalSessionId]);

  const handleCloseTab = useCallback(async (tabId: string) => {
    let currentTabs = tabsRef.current;
    let closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
    if (closedIndex === -1) return;

    const closingTab = currentTabs[closedIndex];
    if (closingTab.kind === 'plugin') {
      try {
        await runPluginBeforeClose(closingTab.id);
      } catch (error) {
        console.error('插件关闭前处理失败：', error);
        return;
      }
      currentTabs = tabsRef.current;
      closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
      if (closedIndex === -1) return;
    }
    if (isWebviewPluginTab(closingTab) && closingTab.plugin_id) {
      try {
        await callWebviewPlugin(
          closingTab.plugin_id,
          terminalSessionId,
          'webview.tabClose',
          { tab_id: closingTab.id },
        );
      } catch (error) {
        console.error('关闭 webview 插件标签失败：', error);
        return;
      }
      currentTabs = tabsRef.current;
      closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
      if (closedIndex === -1) return;
    }
    // 普通 plugin（三方 App）实例无后端运行时，仅移除前端状态与持久化引用。

    const nextTabs = currentTabs.filter((tab) => tab.id !== tabId);
    const currentActiveId = activeTabIdRef.current;
    const nextActiveId = currentActiveId === tabId
      ? pickNextActiveTab(nextTabs, closedIndex) ?? ''
      : normalizeActiveTabId(nextTabs, currentActiveId);

    if (currentActiveId === tabId && isWebviewPluginTab(closingTab) && closingTab.plugin_id) {
      const nextActiveTab = nextTabs.find((tab) => tab.id === nextActiveId);
      const remainsInSameWebviewPlugin = Boolean(
        nextActiveTab
        && isWebviewPluginTab(nextActiveTab)
        && nextActiveTab.plugin_id === closingTab.plugin_id,
      );
      if (!remainsInSameWebviewPlugin) {
        await callWebviewPlugin(
          closingTab.plugin_id,
          terminalSessionId,
          'webview.hide',
        ).catch(console.error);
      }
    }

    tabsRef.current = nextTabs;
    activeTabIdRef.current = nextActiveId;
    setTabs(nextTabs);
    setActiveTabId(nextActiveId);

    if (nextTabs.length === 0) {
      const sessionId = activeSessionIdRef.current;
      if (sessionId) {
        void api.setSessionTabs(sessionId, [], null).catch(console.error);
      }
      // 拓展区三态：全部 tab 关闭后回到 App 矩阵态（面板保持展开）；
      // 未提供矩阵回调时沿用旧行为直接收起。
      if (onShowMatrix) {
        onShowMatrix();
      } else {
        onClose();
      }
      return;
    }
  }, [onClose, onShowMatrix, terminalSessionId]);

  const handleCloseWorkspace = useCallback(() => {
    hideWebviewPluginTabs(tabsRef.current);
    onClose();
  }, [hideWebviewPluginTabs, onClose]);

  // 宿主（矩阵右键菜单）下发的实例命令：关闭全部按 id 快照逐个走
  // handleCloseTab（末 tab 关闭会触发回矩阵/收起的既有逻辑，处于矩阵态时
  // onShowMatrix 幂等）；open-plugin 按 open_mode 分派——单例聚焦已有
  // 实例，多例每次新建。
  const lastAppCommandVersionRef = useRef(0);
  useEffect(() => {
    if (!appCommand || appCommand.version === lastAppCommandVersionRef.current) return;
    // 首次由工具自动拉起拓展区时，会话切换、标签恢复与 app.open 可能
    // 同时发生。只在命令所属会话仍为当前会话且恢复完成后消费，避免刚
    // 创建的 App 标签被稍后到达的恢复结果覆盖。
    if (appCommand.sessionId && appCommand.sessionId !== terminalSessionId) return;
    if (!terminalSessionId || hydratedSessionRef.current !== terminalSessionId) return;
    lastAppCommandVersionRef.current = appCommand.version;
    if (appCommand.action === 'open-plugin' && appCommand.app) {
      const { pluginId, contributionId, title, sandbox, multi, instanceId, focusExisting } =
        appCommand.app;
      if (sandbox === 'webview') {
        void openWebviewPluginTab(appCommand.app, instanceId);
        return;
      }
      // 调用方指定了实例编号时按编号幂等聚焦（重开同一实例）；未指定时
      // multi 且非聚焦语义（矩阵新建实例）才跳过查重直接新建。
      const existing = instanceId
        ? tabsRef.current.find((tab) => tab.id === instanceId)
        : multi && !focusExisting
          ? null
          : tabsRef.current.find(
            (tab) => tab.kind === 'plugin'
              && tab.plugin_id === pluginId
              && tab.contribution_id === contributionId,
          );
      if (existing) {
        activeTabIdRef.current = existing.id;
        setActiveTabId(existing.id);
        return;
      }
      const nextTab: TabState = {
        id: instanceId ?? `plugin-${crypto.randomUUID()}`,
        kind: 'plugin',
        title,
        url: '',
        created_at: new Date().toISOString(),
        plugin_id: pluginId,
        contribution_id: contributionId,
        sandbox,
      };
      const nextTabs = [...tabsRef.current, nextTab];
      tabsRef.current = nextTabs;
      activeTabIdRef.current = nextTab.id;
      setTabs(nextTabs);
      setActiveTabId(nextTab.id);
      return;
    }
    if (appCommand.action === 'close-plugin' && appCommand.app?.pluginId) {
      // app.close 原语落地：instanceId 精确关一个实例，缺省关闭该插件在
      // 本会话的全部实例（宿主已校验调用方显式声明 all）。
      const pluginId = appCommand.app.pluginId;
      const targetIds = appCommand.app.instanceId
        ? tabsRef.current
          .filter((tab) => tab.id === appCommand.app!.instanceId
            && tab.kind === 'plugin'
            && tab.plugin_id === pluginId)
          .map((tab) => tab.id)
        : tabsRef.current
          .filter((tab) => tab.kind === 'plugin' && tab.plugin_id === pluginId)
          .map((tab) => tab.id);
      void (async () => {
        for (const tabId of targetIds) {
          await handleCloseTab(tabId);
        }
      })();
      return;
    }
    const targetIds = tabsRef.current
      .filter((tab) => tab.kind === appCommand.kind)
      .map((tab) => tab.id);
    void (async () => {
      for (const tabId of targetIds) {
        await handleCloseTab(tabId);
      }
    })();
  }, [
    activationRetryVersion,
    appCommand,
    handleCloseTab,
    hydrateVersion,
    openWebviewPluginTab,
    terminalSessionId,
  ]);

  useEffect(() => {
    if (persistTimerRef.current !== null) {
      window.clearTimeout(persistTimerRef.current);
      persistTimerRef.current = null;
    }

    if (hydratingSessionRef.current !== null || !activeSessionId) return;
    if (hydratedSessionRef.current !== activeSessionId) return;
    if (tabs.length === 0) return;

    const tabsToPersist = tabs;
    const activeTabIdToPersist = activeTabId || tabsToPersist[0]?.id || null;
    persistTimerRef.current = window.setTimeout(() => {
      void api.setSessionTabs(activeSessionId, tabsToPersist, activeTabIdToPersist).catch(console.error);
      persistTimerRef.current = null;
    }, TABS_PERSIST_DEBOUNCE_MS);

    return () => {
      if (persistTimerRef.current !== null) {
        window.clearTimeout(persistTimerRef.current);
        persistTimerRef.current = null;
      }
    };
  }, [activeSessionId, activeTabId, tabs]);

  useEffect(() => {
    const handlePageHide = () => persistTabsNow();
    window.addEventListener('pagehide', handlePageHide);
    window.addEventListener('beforeunload', handlePageHide);
    document.addEventListener('visibilitychange', handlePageHide);
    return () => {
      persistTabsNow();
      window.removeEventListener('pagehide', handlePageHide);
      window.removeEventListener('beforeunload', handlePageHide);
      document.removeEventListener('visibilitychange', handlePageHide);
    };
  }, [persistTabsNow]);


  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
          {onShowMatrix && (
            <Button
              size="sm"
              variant="ghost"
              className={`h-7 w-7 shrink-0 p-0 ${
                mode === 'matrix'
                  ? 'bg-muted text-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
              onClick={onShowMatrix}
              title="启动台（回到拓展区矩阵）"
              aria-label="启动台"
            >
              <Grid3x3 className="h-3.5 w-3.5" />
            </Button>
          )}
          {tabs.map((tab) => {
            const active = tab.id === activeTab?.id;
            const webviewPlugin = isWebviewPluginTab(tab);
            const Icon = webviewPlugin ? Globe : Puzzle;
            return (
              <ContextMenu key={tab.id}>
                <ContextMenuTrigger asChild>
                  <div
                    className={`group flex h-7 min-w-28 max-w-44 shrink-0 cursor-default items-center gap-1.5 rounded px-2 text-xs transition-colors ${
                      active
                        ? 'bg-muted text-foreground'
                        : 'text-muted-foreground hover:bg-muted/50 hover:text-foreground'
                    }`}
                    title={tab.title}
                  >
                    <button
                      type="button"
                      className="flex min-w-0 flex-1 items-center gap-1.5"
                      onClick={() => handleSwitchTab(tab.id)}
                    >
                      <Icon className="h-3.5 w-3.5 shrink-0" />
                      <span className="min-w-0 flex-1 truncate text-left">{tab.title}</span>
                    </button>
                    <button
                      type="button"
                      className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-background hover:text-destructive"
                      onClick={(event) => {
                        event.stopPropagation();
                        void handleCloseTab(tab.id);
                      }}
                      title="关闭"
                      aria-label="关闭标签页"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                </ContextMenuTrigger>
                <ContextMenuContent>
                  {webviewPlugin && (
                    <ContextMenuItem onClick={() => {
                      if (tab.plugin_id && tab.contribution_id) {
                        void openWebviewPluginTab({
                          pluginId: tab.plugin_id,
                          contributionId: tab.contribution_id,
                          title: tab.plugin_id === 'browser' ? '浏览器' : tab.title,
                          sandbox: 'webview',
                          multi: true,
                        });
                      }
                    }}>
                      新建浏览器标签页
                    </ContextMenuItem>
                  )}
                  {tabs.length > 1 && webviewPlugin && (
                    <ContextMenuItem
                      onClick={() => {
                        const others = tabsRef.current.filter((item) => (
                          item.id !== tab.id
                          && item.plugin_id === tab.plugin_id
                          && item.contribution_id === tab.contribution_id
                        ));
                        void (async () => {
                          for (const other of others) {
                            await handleCloseTab(other.id);
                          }
                        })();
                      }}
                    >
                      关闭其他标签页
                    </ContextMenuItem>
                  )}
                  <ContextMenuSeparator className="my-1" />
                  <ContextMenuItem onClick={() => void handleCloseTab(tab.id)}>
                    关闭标签页
                  </ContextMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            );
          })}
        </div>

        <Button
          size="sm"
          variant="ghost"
          className="h-7 w-7 shrink-0 p-0 text-muted-foreground hover:text-foreground"
          onClick={handleCloseWorkspace}
          title="关闭工作区"
          aria-label="关闭工作区"
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>

      {/* App 实例内容：矩阵态隐藏保活（切换矩阵不销毁插件实例） */}
      <div className={mode === 'matrix' ? 'hidden' : 'min-h-0 flex-1'}>
        {tabs.map((tab) => (
          tab.sandbox === 'native' && tab.plugin_id === '__builtin__' ? (
            // 官方内置 App 的 native 容器（设计 6.2 ③，仅官方）：按贡献分派组件
            <div
              key={`${terminalSessionId}:${tab.id}`}
              className={
                isVisible && mode === 'app' && tab.id === activeTab?.id
                  ? 'flex h-full min-h-0 w-full flex-1 flex-col'
                  : 'hidden'
              }
            >
              {tab.contribution_id === 'agent-team' ? (
                <AgentTeamPanel />
              ) : (
                <div className="p-4 text-sm text-muted-foreground">
                  该官方 App 内容组件未注册。
                </div>
              )}
            </div>
          ) : (
            <PluginAppTabContent
              key={`${terminalSessionId}:${tab.id}`}
              tab={tab}
              isActive={isVisible && mode === 'app' && tab.id === activeTab?.id}
              sessionId={terminalSessionId || null}
              onRequestNew={isWebviewPluginTab(tab) && tab.plugin_id && tab.contribution_id
                ? () => void openWebviewPluginTab({
                  pluginId: tab.plugin_id!,
                  contributionId: tab.contribution_id!,
                  title: tab.plugin_id === 'browser' ? '浏览器' : tab.title,
                  sandbox: 'webview',
                  multi: true,
                })
                : undefined}
            />
          )
        ))}
      </div>
      {mode === 'matrix' && matrix && (
        <div className="min-h-0 flex-1">{matrix}</div>
      )}
    </div>
  );
}
