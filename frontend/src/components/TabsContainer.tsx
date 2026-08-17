import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Globe, Grid3x3, TerminalSquare, X } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { api } from '@/api/tauri';
import { BUILTIN_TAB_KIND_MULTI, TAB_KIND_NAME } from '@/api/tauri';
import type { TabKind, TabState } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { BrowserTabContent } from './BrowserTabContent';
import { TerminalTabContent } from './TerminalTabContent';
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
  requestedTerminalTabId?: string | null;
  terminalSyncVersion?: number;
  onClose: () => void;
  /** 点击启动台按钮：拓展区切回 App 矩阵态（面板保持展开）。 */
  onShowMatrix?: () => void;
  /** tab 集合（按类型）变化通知：宿主「已打开」绿点的即时数据源，
   *  覆盖新建/关闭/会话恢复，且不受持久化时序影响（新对话未落盘也生效）。 */
  onTabKindsChanged?: (kinds: TabKind[]) => void;
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
const BUSY_TERMINAL_PHASES = new Set(['UserActive', 'Running', 'Interactive']);

/** 宿主（矩阵菜单等）下发的 App 实例命令，version 递增触发执行。 */
export interface AppTabCommand {
  kind: TabKind;
  action: 'new' | 'close-all';
  version: number;
}

function nowText(): string {
  return new Date().toISOString();
}

function newLocalTabId(kind: TabKind): string {
  return `${kind}-${crypto.randomUUID()}`;
}

function createLocalTab(kind: TabKind, id = newLocalTabId(kind)): TabState {
  const createdAt = nowText();
  return {
    id,
    kind,
    title: kind === 'browser' ? '浏览器' : '终端',
    url: kind === 'browser' ? DEFAULT_BROWSER_URL : '',
    created_at: createdAt,
  };
}

function createBrowserTabFromBackend(tab: { id: string; url: string; title: string }): TabState {
  return {
    id: tab.id,
    kind: 'browser',
    title: tab.title || '浏览器',
    url: tab.url || DEFAULT_BROWSER_URL,
    created_at: nowText(),
  };
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
  requestedTerminalTabId,
  terminalSyncVersion = 0,
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
  const workspaceDir = useStore((state) => state.workspaceDir);
  const sessionCwd = useStore((state) => state.sessionCwd);
  const [tabs, setTabs] = useState<TabState[]>([]);
  const [activeTabId, setActiveTabId] = useState('');
  const [hydrateVersion, setHydrateVersion] = useState(0);
  const [activationRetryVersion, setActivationRetryVersion] = useState(0);
  const tabsRef = useRef<TabState[]>([]);
  const activeTabIdRef = useRef('');
  const activeSessionIdRef = useRef<string | null>(null);
  const initialTabKindRef = useRef<TabKind>(initialTabKind);
  const requestedTerminalTabIdRef = useRef<string | null>(requestedTerminalTabId ?? null);
  const isVisibleRef = useRef(isVisible);
  const lastInitialActivationKeyRef = useRef<string | null>(null);
  const hydratingSessionRef = useRef<string | null>(null);
  const hydratedSessionRef = useRef<string | null>(null);
  const persistTimerRef = useRef<number | null>(null);
  const browserMergeRequestRef = useRef(0);
  const terminalSessionIdRef = useRef('');
  const terminalSessionId = activeSessionId || newConversationId || '';
  terminalSessionIdRef.current = terminalSessionId;

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null,
    [activeTabId, tabs],
  );

  // tab 类型集合变化 → 通知宿主更新「已打开」绿点（即时、无持久化时序依赖）
  const onTabKindsChangedRef = useRef(onTabKindsChanged);
  onTabKindsChangedRef.current = onTabKindsChanged;
  const lastTabKindsRef = useRef<string>('');
  useEffect(() => {
    const kinds = Array.from(new Set(tabs.map((tab) => tab.kind)));
    const key = kinds.join(',');
    if (key === lastTabKindsRef.current) return;
    lastTabKindsRef.current = key;
    onTabKindsChangedRef.current?.(kinds);
  }, [tabs]);

  useEffect(() => {
    onActiveKindChange?.(isVisible ? activeTab?.kind ?? null : null);
  }, [activeTab?.kind, isVisible, onActiveKindChange]);

  initialTabKindRef.current = initialTabKind;
  requestedTerminalTabIdRef.current = requestedTerminalTabId ?? null;
  isVisibleRef.current = isVisible;

  const syncBrowserRuntimeForTabs = useCallback(async (
    sessionId: string,
    nextTabs: TabState[],
    nextActiveTabId: string,
  ) => {
    // 只传属于 browser tab 的 active id（terminal tab id 不能作为 browser active）
    const browserActiveId = nextTabs.some(
      tab => tab.kind === 'browser' && tab.id === nextActiveTabId
    ) ? nextActiveTabId : null;
    await api.browserSwitchSession(sessionId, browserActiveId).catch(console.error);
  }, []);

  const restoreRuntimeForTabs = useCallback(async (
    sessionId: string,
    nextTabs: TabState[],
    nextActiveTabId: string,
    visible: boolean,
    browserRuntimeSynced = false,
  ) => {
    if (!browserRuntimeSynced) {
      await syncBrowserRuntimeForTabs(sessionId, nextTabs, nextActiveTabId);
    }

    const terminalTabs = nextTabs.filter((tab) => tab.kind === 'terminal');
    if (terminalTabs.length > 0) {
      let tabsToRestore = terminalTabs;
      try {
        const runtimeTabs = await api.terminalTabList(sessionId);
        const liveRuntimeIds = new Set(
          runtimeTabs.tabs
            .filter((tab) => tab.alive)
            .map((tab) => tab.id),
        );
        tabsToRestore = terminalTabs.filter((tab) => !liveRuntimeIds.has(tab.id));
      } catch {
        tabsToRestore = terminalTabs;
      }
      await Promise.all(tabsToRestore
        .map((tab) => api.terminalTabRestore(
          sessionId,
          tab.id,
          tab.title,
          sessionCwd || workspaceDir || null,
        ).catch(console.error)));
    }

    if (!visible) {
      await api.browserHide(sessionId).catch(console.error);
      return;
    }

    const activeTab = nextTabs.find((tab) => tab.id === nextActiveTabId);
    if (activeTab?.kind === 'terminal') {
      await api.terminalTabSwitch(sessionId, activeTab.id).catch(console.error);
      await api.browserHide(sessionId).catch(console.error);
    } else if (!activeTab || activeTab.kind !== 'browser') {
      await api.browserHide(sessionId).catch(console.error);
    }
  }, [sessionCwd, syncBrowserRuntimeForTabs, workspaceDir]);

  const handleNewTab = useCallback(async (kind: TabKind) => {
    if (kind === 'browser') {
      try {
        const tabId = await api.browserTabNew(terminalSessionId, DEFAULT_BROWSER_URL);
        const nextTab = createLocalTab(kind, tabId);
        const nextTabs = tabsRef.current.some((tab) => tab.id === tabId)
          ? tabsRef.current.map((tab) => tab.id === tabId ? nextTab : tab)
          : [...tabsRef.current, nextTab];
        tabsRef.current = nextTabs;
        activeTabIdRef.current = nextTab.id;
        setTabs(nextTabs);
        setActiveTabId(nextTab.id);
      } catch (err) {
        console.error('新建浏览器 Tab 失败：', err);
      }
      return;
    }

    try {
      void api.browserHide(terminalSessionId).catch(console.error);
      const tabId = await api.terminalTabNew(terminalSessionId, '终端', sessionCwd || workspaceDir || null);
      const nextTab = createLocalTab(kind, tabId);
      const nextTabs = [...tabsRef.current, nextTab];
      tabsRef.current = nextTabs;
      activeTabIdRef.current = nextTab.id;
      setTabs(nextTabs);
      setActiveTabId(nextTab.id);
    } catch (err) {
      console.error('新建终端 Tab 失败：', err);
    }
  }, [sessionCwd, terminalSessionId, workspaceDir]);

  const mergeTerminalRuntimeTabs = useCallback(async (preferredTabId?: string | null) => {
    try {
      const result = await api.terminalTabList(terminalSessionId);
      if (result.tabs.length === 0) return false;

      const terminalTabs = result.tabs.map((tab) => ({
        id: tab.id,
        kind: 'terminal' as const,
        title: tab.title || '终端',
        url: '',
        created_at: tab.created_at || nowText(),
        phase: tab.phase,
      }));
      const preferredExists = Boolean(
        preferredTabId && terminalTabs.some((tab) => tab.id === preferredTabId),
      );
      const nextActiveId = (preferredExists ? preferredTabId : null)
        || result.active_tab_id
        || terminalTabs[0]?.id
        || '';

      const nextTabs = (() => {
        const currentTabs = tabsRef.current;
        const terminalIds = new Set(terminalTabs.map((tab) => tab.id));
        const nonTerminalTabs = currentTabs.filter((tab) => (
          tab.kind !== 'terminal' || terminalIds.has(tab.id)
        ));
        const existingIds = new Set(nonTerminalTabs.map((tab) => tab.id));
        const updatedTabs = nonTerminalTabs.map((tab) => {
          if (tab.kind !== 'terminal') return tab;
          const backendTab = terminalTabs.find((item) => item.id === tab.id);
          return backendTab ? {
            ...tab,
            title: backendTab.title,
            created_at: backendTab.created_at,
            phase: backendTab.phase,
          } : tab;
        });
        const newTabs = terminalTabs.filter((tab) => !existingIds.has(tab.id));
        return [...updatedTabs, ...newTabs];
      })();

      tabsRef.current = nextTabs;
      setTabs(nextTabs);

      const shouldActivate = isVisibleRef.current
        && initialTabKindRef.current === 'terminal'
        && (
          preferredTabId
            ? requestedTerminalTabIdRef.current === preferredTabId
            : requestedTerminalTabIdRef.current === null
        );

      if (nextActiveId && shouldActivate) {
        void api.browserHide(terminalSessionId).catch(console.error);
        if (result.active_tab_id !== nextActiveId) {
          void api.terminalTabSwitch(terminalSessionId, nextActiveId).catch(console.error);
        }
        activeTabIdRef.current = nextActiveId;
        setActiveTabId(nextActiveId);
      }
      if (activeSessionIdRef.current === terminalSessionId) {
        const activeId = shouldActivate && nextActiveId
          ? nextActiveId
          : activeTabIdRef.current || nextActiveId || nextTabs[0]?.id || null;
        void api.setSessionTabs(terminalSessionId, nextTabs, activeId).catch(console.error);
      }
      return true;
    } catch (err) {
      console.error('同步终端 Tab 失败：', err);
      return false;
    }
  }, [terminalSessionId]);

  const mergeBrowserRuntimeTabs = useCallback(async () => {
    if (!terminalSessionId) return;
    const requestedSessionId = terminalSessionId;
    const requestId = ++browserMergeRequestRef.current;
    const result = await api.browserTabList(requestedSessionId);
    if (
      requestId !== browserMergeRequestRef.current
      || terminalSessionIdRef.current !== requestedSessionId
    ) {
      return;
    }
    const browserTabs = result.tabs.map(createBrowserTabFromBackend);
    const browserIds = new Set(browserTabs.map((tab) => tab.id));
    const backendById = new Map(browserTabs.map((tab) => [tab.id, tab]));
    const currentTabs = tabsRef.current;
    const retainedTabs = currentTabs
      .filter((tab) => tab.kind !== 'browser' || browserIds.has(tab.id))
      .map((tab) => {
        if (tab.kind !== 'browser') return tab;
        const backendTab = backendById.get(tab.id);
        return backendTab ? { ...tab, title: backendTab.title, url: backendTab.url } : tab;
      });
    const retainedIds = new Set(retainedTabs.map((tab) => tab.id));
    const addedTabs = browserTabs.filter((tab) => !retainedIds.has(tab.id));
    const nextTabs = [...retainedTabs, ...addedTabs];

    const currentActive = currentTabs.find((tab) => tab.id === activeTabIdRef.current);
    let nextActiveId = activeTabIdRef.current;
    if (result.active_tab_id && (!currentActive || currentActive.kind === 'browser')) {
      nextActiveId = result.active_tab_id;
    } else if (!nextTabs.some((tab) => tab.id === nextActiveId)) {
      nextActiveId = nextTabs[0]?.id || '';
    }

    tabsRef.current = nextTabs;
    activeTabIdRef.current = nextActiveId;
    setTabs(nextTabs);
    setActiveTabId(nextActiveId);
  }, [terminalSessionId]);



  useEffect(() => {
    activeTabIdRef.current = activeTabId;
  }, [activeTabId]);

  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void listen<{ session_id?: string }>('browser:tab_updated', (event) => {
      if (cancelled || event.payload?.session_id !== terminalSessionId) return;
      void mergeBrowserRuntimeTabs().catch(console.error);
    }).then((cleanup) => {
      if (cancelled) {
        cleanup();
      } else {
        unlisten = cleanup;
      }
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [mergeBrowserRuntimeTabs, terminalSessionId]);

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
        const nextTabs = sessionTabs.tabs;
        const nextActiveTabId = normalizeActiveTabId(nextTabs, sessionTabs.active_tab_id);
        await syncBrowserRuntimeForTabs(sessionId, nextTabs, nextActiveTabId);
        if (cancelled) return;

        tabsRef.current = nextTabs;
        activeTabIdRef.current = nextActiveTabId;
        setTabs(nextTabs);
        setActiveTabId(nextActiveTabId);

        // await runtime 恢复完成后再标记已 hydrate（避免 activateOrCreateTab 竞态）
        await restoreRuntimeForTabs(sessionId, nextTabs, nextActiveTabId, isVisibleRef.current, true);
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
        await api.browserSwitchSession(sessionId, null).catch(console.error);
        // 失败时清除 hydrating，允许下次重试
        if (hydratingSessionRef.current === sessionId) {
          hydratingSessionRef.current = null;
        }
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
  }, [activeSessionId, newConversationId, restoreRuntimeForTabs, syncBrowserRuntimeForTabs]);

  const activateOrCreateTab = useCallback(async (kind: TabKind) => {
    const sessionId = terminalSessionId;
    if (!sessionId) return;

    const currentTabs = tabsRef.current;
    const currentActiveId = activeTabIdRef.current;
    const currentActiveTab = currentTabs.find((tab) => tab.id === currentActiveId);

    // 当前已是目标类型：no-op（面板可见性由 openWorkspacePanel 保证）
    if (currentActiveTab?.kind === kind) {
      return;
    }

    if (kind === 'browser') {
      // 已有 browser tab → 切换，不创建
      const existingBrowserTab = currentTabs.find((tab) => tab.kind === 'browser');
      if (existingBrowserTab) {
        await api.browserTabSwitch(sessionId, existingBrowserTab.id).catch(console.error);
        activeTabIdRef.current = existingBrowserTab.id;
        setActiveTabId(existingBrowserTab.id);
        return;
      }
      // 无 browser tab → 从 runtime 查一次（可能有未同步的）
      try {
        const result = await api.browserTabList(sessionId);
        if (result.tabs.length > 0) {
          const browserTabs = result.tabs.map(createBrowserTabFromBackend);
          const activeId = result.active_tab_id || browserTabs[0]?.id || '';
          const currentIds = new Set(currentTabs.map((tab) => tab.id));
          const newTabs = browserTabs.filter((tab) => !currentIds.has(tab.id));
          const updatedTabs = currentTabs.map((tab) => {
            const backendTab = browserTabs.find((item) => item.id === tab.id);
            return backendTab ? { ...tab, title: backendTab.title, url: backendTab.url } : tab;
          });
          const merged = [...updatedTabs, ...newTabs];
          tabsRef.current = merged;
          setTabs(merged);
          activeTabIdRef.current = activeId;
          setActiveTabId(activeId);
          return;
        }
      } catch {
        // 运行时未初始化，继续创建
      }
      // 创建新 browser tab
      await handleNewTab(kind);
      return;
    }

    // terminal：先检查 workspace 已有 tab（避免每次都查 runtime）
    const existingTerminalTab = currentTabs.find((tab) => tab.kind === 'terminal');
    if (existingTerminalTab) {
      await api.browserHide(sessionId).catch(console.error);
      await api.terminalTabSwitch(sessionId, existingTerminalTab.id).catch(console.error);
      activeTabIdRef.current = existingTerminalTab.id;
      setActiveTabId(existingTerminalTab.id);
      return;
    }

    // workspace 没有 → 从 runtime 查一次
    if (await mergeTerminalRuntimeTabs(requestedTerminalTabId)) {
      return;
    }

    // 创建新 terminal tab
    await api.browserHide(sessionId).catch(console.error);
    await handleNewTab(kind);
  }, [handleNewTab, mergeTerminalRuntimeTabs, requestedTerminalTabId, terminalSessionId]);

  useEffect(() => {
    if (!isVisible) return;
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
    openRequestVersion,
    terminalSessionId,
  ]);

  useEffect(() => {
    if (!isVisible || !requestedTerminalTabId) return;
    if (tabsRef.current.some((tab) => tab.id === requestedTerminalTabId)) return;
    void mergeTerminalRuntimeTabs(requestedTerminalTabId);
  }, [isVisible, mergeTerminalRuntimeTabs, requestedTerminalTabId]);

  useEffect(() => {
    if (!isVisible || initialTabKind !== 'terminal') return;
    if (tabsRef.current.some((tab) => tab.kind === 'terminal')) return;
    void mergeTerminalRuntimeTabs(requestedTerminalTabId ?? null);
  }, [initialTabKind, isVisible, mergeTerminalRuntimeTabs, requestedTerminalTabId, openRequestVersion]);

  useEffect(() => {
    if (!activeSessionId || terminalSyncVersion === 0) return;
    // 终端 tab 更新只合并 terminal tabs，保留 browser tabs（不全量 refresh 覆盖）
    void mergeTerminalRuntimeTabs();
  }, [activeSessionId, mergeTerminalRuntimeTabs, terminalSyncVersion]);

  const handleSwitchTab = useCallback((tabId: string) => {
    const nextTab = tabs.find((tab) => tab.id === tabId);
    if (nextTab?.kind === 'terminal') {
      void api.browserHide(terminalSessionId).catch(console.error);
      void api.terminalTabSwitch(terminalSessionId, tabId).catch(console.error);
    } else if (nextTab?.kind === 'browser') {
      void api.browserTabSwitch(terminalSessionId, tabId).catch(console.error);
    }
    activeTabIdRef.current = tabId;
    setActiveTabId(tabId);
  }, [tabs, terminalSessionId]);

  const handleCloseTab = useCallback((tabId: string) => {
    const currentTabs = tabsRef.current;
    const closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
    if (closedIndex === -1) return;

    const closingTab = currentTabs[closedIndex];
    if (closingTab.kind === 'browser') {
      void api.browserTabClose(terminalSessionId, tabId).catch(console.error);
    } else {
      void api.terminalTabClose(terminalSessionId, tabId).catch(console.error);
    }

    const nextTabs = currentTabs.filter((tab) => tab.id !== tabId);
    const currentActiveId = activeTabIdRef.current;
    const nextActiveId = currentActiveId === tabId
      ? pickNextActiveTab(nextTabs, closedIndex) ?? ''
      : normalizeActiveTabId(nextTabs, currentActiveId);
    tabsRef.current = nextTabs;
    activeTabIdRef.current = nextActiveId;
    setTabs(nextTabs);
    setActiveTabId(nextActiveId);

    if (nextTabs.length === 0) {
      const sessionId = activeSessionIdRef.current;
      if (sessionId) {
        void api.setSessionTabs(sessionId, [], null).catch(console.error);
      }
      // 最后一个 tab 关闭：显式隐藏浏览器面板（webview off-screen + visible=false）
      void api.browserHide(terminalSessionId).catch(console.error);
      void api.browserSwitchSession(terminalSessionId, null).catch(console.error);
      // 拓展区三态：全部 tab 关闭后回到 App 矩阵态（面板保持展开）；
      // 未提供矩阵回调时沿用旧行为直接收起。
      if (onShowMatrix) {
        onShowMatrix();
      } else {
        onClose();
      }
      return;
    }

    if (currentActiveId === tabId) {
      const nextActiveTab = nextTabs.find((tab) => tab.id === nextActiveId);
      if (nextActiveTab?.kind !== 'browser') {
        void api.browserHide(terminalSessionId).catch(console.error);
      }
    }
  }, [onClose, onShowMatrix, terminalSessionId]);

  const handleCloseWorkspace = useCallback(() => {
    void api.browserHide(terminalSessionId).catch(console.error);
    onClose();
  }, [onClose]);

  // 宿主（矩阵右键菜单）下发的实例命令：新建走既有 handleNewTab（宿主并行
  // 切换 App 态）；关闭全部按 id 快照逐个走 handleCloseTab（末 tab 关闭会
  // 触发回矩阵/收起的既有逻辑，处于矩阵态时 onShowMatrix 幂等）。
  const lastAppCommandVersionRef = useRef(0);
  useEffect(() => {
    if (!appCommand || appCommand.version === lastAppCommandVersionRef.current) return;
    lastAppCommandVersionRef.current = appCommand.version;
    if (appCommand.action === 'new') {
      void handleNewTab(appCommand.kind);
      return;
    }
    const targetIds = tabsRef.current
      .filter((tab) => tab.kind === appCommand.kind)
      .map((tab) => tab.id);
    for (const tabId of targetIds) {
      handleCloseTab(tabId);
    }
  }, [appCommand, handleCloseTab, handleNewTab]);

  const handleBrowserMetadataChange = useCallback((
    tabId: string,
    metadata: { title?: string; url?: string },
  ) => {
    const nextTabs = tabsRef.current.map((tab) => {
      if (tab.id !== tabId) return tab;
      return {
        ...tab,
        title: metadata.title || tab.title,
        url: metadata.url ?? tab.url,
      };
    });
    tabsRef.current = nextTabs;
    setTabs(nextTabs);
  }, []);

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
            const Icon = tab.kind === 'browser' ? Globe : TerminalSquare;
            const busy = tab.kind === 'terminal'
              && Boolean(tab.phase && BUSY_TERMINAL_PHASES.has(tab.phase));
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
                      {busy && (
                        <span
                          className="h-2 w-2 shrink-0 rounded-full bg-yellow-400 ring-1 ring-yellow-600/40"
                          title="终端繁忙"
                          aria-label="终端繁忙"
                        />
                      )}
                      <span className="min-w-0 flex-1 truncate text-left">{tab.title}</span>
                    </button>
                    <button
                      type="button"
                      className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-background hover:text-destructive"
                      onClick={(event) => {
                        event.stopPropagation();
                        handleCloseTab(tab.id);
                      }}
                      title="关闭"
                      aria-label="关闭标签页"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </div>
                </ContextMenuTrigger>
                <ContextMenuContent>
                  {/* 多实例 App 才提供新建；单实例（浏览器）重复打开聚焦已有，不支持多开 */}
                  {BUILTIN_TAB_KIND_MULTI[tab.kind] && (
                    <ContextMenuItem onClick={() => void handleNewTab(tab.kind)}>
                      新建{TAB_KIND_NAME[tab.kind]}标签页
                    </ContextMenuItem>
                  )}
                  {tabs.length > 1 && BUILTIN_TAB_KIND_MULTI[tab.kind] && (
                    <ContextMenuItem
                      onClick={() => {
                        for (const other of tabsRef.current.filter((item) => item.id !== tab.id)) {
                          handleCloseTab(other.id);
                        }
                      }}
                    >
                      关闭其他标签页
                    </ContextMenuItem>
                  )}
                  <ContextMenuSeparator className="my-1" />
                  <ContextMenuItem onClick={() => handleCloseTab(tab.id)}>
                    关闭标签页
                  </ContextMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            );
          })}
          <Button
            size="sm"
            variant="ghost"
            className="h-7 w-7 shrink-0 p-0"
            onClick={() => handleNewTab('terminal')}
            title="新建终端"
            aria-label="新建终端标签页"
          >
            <TerminalSquare className="h-3.5 w-3.5" />
          </Button>
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

      {/* App 实例内容：矩阵态隐藏保活（切换矩阵不销毁浏览器/终端实例） */}
      <div className={mode === 'matrix' ? 'hidden' : 'min-h-0 flex-1'}>
        {tabs.map((tab) => (
          tab.kind === 'terminal' ? (
            <TerminalTabContent
              key={`${terminalSessionId}:${tab.id}`}
              sessionId={terminalSessionId}
              tabId={tab.id}
              isActive={isVisible && mode === 'app' && tab.id === activeTab?.id}
            />
          ) : (
            <BrowserTabContent
              key={`${terminalSessionId}:${tab.id}`}
              sessionId={terminalSessionId}
              tabId={tab.id}
              initialUrl={tab.url}
              isActive={isVisible && mode === 'app' && tab.id === activeTab?.id}
              onMetadataChange={handleBrowserMetadataChange}
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
