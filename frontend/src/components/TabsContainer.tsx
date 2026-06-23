import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Globe, Plus, TerminalSquare, X } from 'lucide-react';
import { api } from '@/api/tauri';
import type { TabKind, TabState } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { BrowserTabContent } from './BrowserTabContent';
import { TerminalTabContent } from './TerminalTabContent';
import { Button } from './ui/button';

interface TabsContainerProps {
  initialTabKind: TabKind;
  isVisible: boolean;
  openRequestVersion: number;
  onClose: () => void;
}

const DEFAULT_BROWSER_URL = 'about:blank';
const DRAFT_TERMINAL_ID = '__draft_terminal__';
const TABS_PERSIST_DEBOUNCE_MS = 500;

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

function browserRuntimeTabs(tabs: TabState[]): Array<{ id: string; url: string; title: string }> {
  return tabs
    .filter((tab) => tab.kind === 'browser')
    .map((tab) => ({
      id: tab.id,
      url: tab.url || DEFAULT_BROWSER_URL,
      title: tab.title || '浏览器',
    }));
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
}: TabsContainerProps) {
  const activeSessionId = useStore((state) => state.activeSessionId);
  const draftTerminalId = useStore((state) => state.draftTerminalId);
  const workspaceTabsTransfer = useStore((state) => state.workspaceTabsTransfer);
  const workspaceDir = useStore((state) => state.workspaceDir);
  const sessionCwd = useStore((state) => state.sessionCwd);
  const [tabs, setTabs] = useState<TabState[]>([]);
  const [activeTabId, setActiveTabId] = useState('');
  const [hydrateVersion, setHydrateVersion] = useState(0);
  const tabsRef = useRef<TabState[]>([]);
  const lastInitialActivationKeyRef = useRef<string | null>(null);
  const lastHydratedSessionRef = useRef<string | null>(null);
  const hydratingRef = useRef(false);
  const persistTimerRef = useRef<number | null>(null);
  const transferVersionRef = useRef<number | null>(null);
  const terminalSessionId = activeSessionId || draftTerminalId || DRAFT_TERMINAL_ID;

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null,
    [activeTabId, tabs],
  );

  const restoreRuntimeForTabs = useCallback(async (
    sessionId: string,
    nextTabs: TabState[],
    nextActiveTabId: string,
    visible: boolean,
  ) => {
    const browserTabs = browserRuntimeTabs(nextTabs);
    await api.browserSwitchSession(
      sessionId,
      browserTabs,
      browserTabs.some((tab) => tab.id === nextActiveTabId) ? nextActiveTabId : null,
    ).catch(console.error);

    await Promise.all(nextTabs
      .filter((tab) => tab.kind === 'terminal')
      .map((tab) => api.terminalTabRestore(sessionId, tab.id, tab.title).catch(console.error)));

    if (!visible) {
      await api.browserHide().catch(console.error);
      return;
    }

    const activeTab = nextTabs.find((tab) => tab.id === nextActiveTabId);
    if (activeTab?.kind === 'terminal') {
      await api.terminalTabSwitch(sessionId, activeTab.id).catch(console.error);
      await api.browserHide().catch(console.error);
    } else if (!activeTab || activeTab.kind !== 'browser') {
      await api.browserHide().catch(console.error);
    }
  }, []);

  const handleNewTab = useCallback(async (kind: TabKind) => {
    if (kind === 'browser') {
      try {
        const tabId = await api.browserTabNew(DEFAULT_BROWSER_URL);
        const nextTab = createLocalTab(kind, tabId);
        setTabs((currentTabs) => [...currentTabs, nextTab]);
        setActiveTabId(nextTab.id);
      } catch (err) {
        console.error('新建浏览器 Tab 失败：', err);
      }
      return;
    }

    try {
      void api.browserHide().catch(console.error);
      const tabId = await api.terminalTabNew(terminalSessionId, '终端', sessionCwd || workspaceDir || null);
      const nextTab = createLocalTab(kind, tabId);
      setTabs((currentTabs) => {
        setActiveTabId(nextTab.id);
        return [...currentTabs, nextTab];
      });
    } catch (err) {
      console.error('新建终端 Tab 失败：', err);
    }
  }, [sessionCwd, terminalSessionId, workspaceDir]);

  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  useEffect(() => {
    if (!activeSessionId) {
      if (lastHydratedSessionRef.current !== DRAFT_TERMINAL_ID) {
        lastHydratedSessionRef.current = DRAFT_TERMINAL_ID;
        setTabs([]);
        setActiveTabId('');
      }
      return;
    }

    if (
      workspaceTabsTransfer?.fromSessionId === DRAFT_TERMINAL_ID
      && workspaceTabsTransfer.toSessionId === activeSessionId
      && transferVersionRef.current !== workspaceTabsTransfer.version
      && tabsRef.current.length > 0
    ) {
      return;
    }

    if (lastHydratedSessionRef.current === activeSessionId) return;
    lastHydratedSessionRef.current = activeSessionId;
    hydratingRef.current = true;

    let cancelled = false;
    const hydrate = async () => {
      try {
        const sessionTabs = await api.getSessionTabs(activeSessionId);
        if (cancelled) return;
        const nextTabs = sessionTabs.tabs;
        const nextActiveTabId = normalizeActiveTabId(nextTabs, sessionTabs.active_tab_id);
        setTabs(nextTabs);
        setActiveTabId(nextActiveTabId);
        await restoreRuntimeForTabs(activeSessionId, nextTabs, nextActiveTabId, isVisible);
      } catch (err) {
        console.error('恢复会话 Tab 失败：', err);
        if (!cancelled) {
          setTabs([]);
          setActiveTabId('');
          await api.browserSwitchSession(activeSessionId, [], null).catch(console.error);
        }
      } finally {
        if (!cancelled) {
          hydratingRef.current = false;
          setHydrateVersion((version) => version + 1);
        }
      }
    };

    void hydrate();
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, isVisible, restoreRuntimeForTabs, workspaceTabsTransfer]);

  const activateOrCreateTab = useCallback(async (kind: TabKind) => {
    const existing = tabsRef.current.find((tab) => tab.kind === kind);
    if (existing) {
      if (existing.kind === 'terminal') {
        void api.browserHide().catch(console.error);
      }
      setActiveTabId(existing.id);
      return;
    }

    if (kind === 'browser') {
      try {
        const result = await api.browserTabList();
        if (result.tabs.length > 0) {
          const browserTabs = result.tabs.map(createBrowserTabFromBackend);
          const activeId = result.active_tab_id || browserTabs[0]?.id || '';
          setTabs((currentTabs) => {
            const currentIds = new Set(currentTabs.map((tab) => tab.id));
            const newTabs = browserTabs.filter((tab) => !currentIds.has(tab.id));
            const updatedTabs = currentTabs.map((tab) => {
              const backendTab = browserTabs.find((item) => item.id === tab.id);
              return backendTab ? { ...tab, title: backendTab.title, url: backendTab.url } : tab;
            });
            return [...updatedTabs, ...newTabs];
          });
          setActiveTabId(activeId);
          return;
        }
      } catch {
        // 浏览器运行时可能尚未初始化，继续按空白 Tab 创建。
      }
    }

    await handleNewTab(kind);
  }, [handleNewTab]);

  useEffect(() => {
    if (!isVisible) return;
    if (hydratingRef.current) return;
    const activationKey = `${terminalSessionId}:${hydrateVersion}:${initialTabKind}:${openRequestVersion}`;
    if (lastInitialActivationKeyRef.current === activationKey) {
      return;
    }
    lastInitialActivationKeyRef.current = activationKey;
    void activateOrCreateTab(initialTabKind);
  }, [
    activateOrCreateTab,
    hydrateVersion,
    initialTabKind,
    isVisible,
    openRequestVersion,
    terminalSessionId,
  ]);

  const handleSwitchTab = useCallback((tabId: string) => {
    const nextTab = tabs.find((tab) => tab.id === tabId);
    if (nextTab?.kind === 'terminal') {
      void api.browserHide().catch(console.error);
      void api.terminalTabSwitch(terminalSessionId, tabId).catch(console.error);
    } else if (nextTab?.kind === 'browser') {
      void api.browserTabSwitch(tabId).catch(console.error);
    }
    setActiveTabId(tabId);
  }, [tabs, terminalSessionId]);

  const handleCloseTab = useCallback((tabId: string) => {
    setTabs((currentTabs) => {
      const closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
      if (closedIndex === -1) return currentTabs;
      const closingTab = currentTabs[closedIndex];
      if (closingTab.kind === 'browser') {
        void api.browserTabClose(tabId).catch(console.error);
      } else {
        void api.terminalTabClose(terminalSessionId, tabId).catch(console.error);
      }
      const nextTabs = currentTabs.filter((tab) => tab.id !== tabId);
      if (nextTabs.length === 0) {
        setActiveTabId('');
        if (activeSessionId) {
          void api.setSessionTabs(activeSessionId, [], null).catch(console.error);
        }
        void api.browserSwitchSession(terminalSessionId, [], null).catch(console.error);
        onClose();
        return [];
      }
      if (activeTabId === tabId) {
        const nextActiveId = pickNextActiveTab(nextTabs, closedIndex) ?? '';
        const nextActiveTab = nextTabs.find((tab) => tab.id === nextActiveId);
        if (nextActiveTab?.kind !== 'browser') {
          void api.browserHide().catch(console.error);
        }
        setActiveTabId(nextActiveId);
      }
      return nextTabs;
    });
  }, [activeTabId, onClose, terminalSessionId]);

  const handleCloseWorkspace = useCallback(() => {
    void api.browserHide().catch(console.error);
    onClose();
  }, [onClose]);

  const handleBrowserMetadataChange = useCallback((
    tabId: string,
    metadata: { title?: string; url?: string },
  ) => {
    setTabs((currentTabs) => currentTabs.map((tab) => {
      if (tab.id !== tabId) return tab;
      return {
        ...tab,
        title: metadata.title || tab.title,
        url: metadata.url ?? tab.url,
      };
    }));
  }, []);

  useEffect(() => {
    if (persistTimerRef.current !== null) {
      window.clearTimeout(persistTimerRef.current);
      persistTimerRef.current = null;
    }

    if (hydratingRef.current || !activeSessionId) return;
    if (lastHydratedSessionRef.current !== activeSessionId) return;
    if (tabs.length === 0) return;

    const tabsToPersist = tabs;
    const activeTabIdToPersist = activeTabId || tabsToPersist[0]?.id || null;
    persistTimerRef.current = window.setTimeout(() => {
      api.setSessionTabs(activeSessionId, tabsToPersist, activeTabIdToPersist).catch(console.error);
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
    if (!workspaceTabsTransfer || transferVersionRef.current === workspaceTabsTransfer.version) {
      return;
    }
    transferVersionRef.current = workspaceTabsTransfer.version;

    const { fromSessionId, toSessionId } = workspaceTabsTransfer;
    if (fromSessionId !== DRAFT_TERMINAL_ID || activeSessionId !== toSessionId) {
      return;
    }

    const currentTabs = tabsRef.current;
    if (currentTabs.length === 0) return;

    const nextActiveTabId = normalizeActiveTabId(currentTabs, activeTabId);
    lastHydratedSessionRef.current = toSessionId;
    hydratingRef.current = true;

    const transfer = async () => {
      try {
        await Promise.all(currentTabs
          .filter((tab) => tab.kind === 'terminal')
          .map(async (tab) => {
            try {
              await api.terminalTabSwitch(toSessionId, tab.id);
            } catch {
              await api.terminalTabRestore(toSessionId, tab.id, tab.title).catch(console.error);
            }
          }));
        const browserTabs = browserRuntimeTabs(currentTabs);
        if (browserTabs.length > 0) {
          await api.browserSwitchSession(
            toSessionId,
            browserTabs,
            browserTabs.some((tab) => tab.id === nextActiveTabId) ? nextActiveTabId : null,
          ).catch(console.error);
        }
        await api.setSessionTabs(toSessionId, currentTabs, nextActiveTabId || null);
      } finally {
        hydratingRef.current = false;
      }
    };

    void transfer();
  }, [activeSessionId, activeTabId, workspaceTabsTransfer]);

  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
          {tabs.map((tab) => {
            const active = tab.id === activeTab?.id;
            const Icon = tab.kind === 'browser' ? Globe : TerminalSquare;
            return (
              <div
                key={tab.id}
                className={`group flex h-7 min-w-28 max-w-44 shrink-0 items-center gap-1.5 rounded px-2 text-xs transition-colors ${
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
                    handleCloseTab(tab.id);
                  }}
                  title="关闭"
                  aria-label="关闭标签页"
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
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
          <Button
            size="sm"
            variant="ghost"
            className="h-7 w-7 shrink-0 p-0"
            onClick={() => handleNewTab('browser')}
            title="新建浏览器"
            aria-label="新建浏览器标签页"
          >
            <Plus className="h-3.5 w-3.5" />
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

      <div className="min-h-0 flex-1">
        {tabs.map((tab) => (
          tab.kind === 'terminal' ? (
            <TerminalTabContent
              key={`${terminalSessionId}:${tab.id}`}
              sessionId={terminalSessionId}
              tabId={tab.id}
              isActive={isVisible && tab.id === activeTab?.id}
            />
          ) : (
            <BrowserTabContent
              key={`${terminalSessionId}:${tab.id}`}
              tabId={tab.id}
              initialUrl={tab.url}
              isActive={isVisible && tab.id === activeTab?.id}
              onMetadataChange={handleBrowserMetadataChange}
            />
          )
        ))}
      </div>
    </div>
  );
}
