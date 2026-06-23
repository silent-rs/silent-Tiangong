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
  onClose: () => void;
}

const DEFAULT_BROWSER_URL = 'about:blank';
const DRAFT_TERMINAL_ID = '__draft_terminal__';

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

function pickNextActiveTab(tabs: TabState[], closedIndex: number): string | null {
  if (tabs.length === 0) return null;
  const nextIndex = Math.min(closedIndex, tabs.length - 1);
  return tabs[nextIndex]?.id ?? tabs[0]?.id ?? null;
}

export function TabsContainer({ initialTabKind, onClose }: TabsContainerProps) {
  const activeSessionId = useStore((state) => state.activeSessionId);
  const draftTerminalId = useStore((state) => state.draftTerminalId);
  const [tabs, setTabs] = useState<TabState[]>([]);
  const [activeTabId, setActiveTabId] = useState('');
  const tabsRef = useRef<TabState[]>([]);
  const lastInitialTabKindRef = useRef<TabKind | null>(null);
  const terminalSessionId = activeSessionId || draftTerminalId || DRAFT_TERMINAL_ID;

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null,
    [activeTabId, tabs],
  );

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

    void api.browserHide().catch(console.error);
    setTabs((currentTabs) => {
      const nextTab = createLocalTab(kind);
      setActiveTabId(nextTab.id);
      return [...currentTabs, nextTab];
    });
  }, []);

  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

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
    if (lastInitialTabKindRef.current === initialTabKind && tabsRef.current.length > 0) {
      return;
    }
    lastInitialTabKindRef.current = initialTabKind;
    void activateOrCreateTab(initialTabKind);
  }, [activateOrCreateTab, initialTabKind]);

  const handleSwitchTab = useCallback((tabId: string) => {
    const nextTab = tabs.find((tab) => tab.id === tabId);
    if (nextTab?.kind === 'terminal') {
      void api.browserHide().catch(console.error);
    }
    setActiveTabId(tabId);
  }, [tabs]);

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

  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
          {tabs.map((tab) => {
            const active = tab.id === activeTab?.id;
            const Icon = tab.kind === 'browser' ? Globe : TerminalSquare;
            return (
              <button
                key={tab.id}
                type="button"
                onClick={() => handleSwitchTab(tab.id)}
                className={`group flex h-7 min-w-28 max-w-44 shrink-0 items-center gap-1.5 rounded px-2 text-xs transition-colors ${
                  active
                    ? 'bg-muted text-foreground'
                    : 'text-muted-foreground hover:bg-muted/50 hover:text-foreground'
                }`}
                title={tab.title}
              >
                <Icon className="h-3.5 w-3.5 shrink-0" />
                <span className="min-w-0 flex-1 truncate text-left">{tab.title}</span>
                <span
                  role="button"
                  tabIndex={0}
                  className="shrink-0 rounded p-0.5 opacity-70 hover:bg-background hover:text-destructive"
                  onClick={(event) => {
                    event.stopPropagation();
                    handleCloseTab(tab.id);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter' || event.key === ' ') {
                      event.preventDefault();
                      event.stopPropagation();
                      handleCloseTab(tab.id);
                    }
                  }}
                  title="关闭"
                >
                  <X className="h-3 w-3" />
                </span>
              </button>
            );
          })}
        </div>

        <Button
          size="sm"
          variant="ghost"
          className="h-7 w-7 shrink-0 p-0"
          onClick={() => handleNewTab('terminal')}
          title="新建终端"
        >
          <TerminalSquare className="h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 w-7 shrink-0 p-0"
          onClick={() => handleNewTab('browser')}
          title="新建浏览器"
        >
          <Plus className="h-3.5 w-3.5" />
        </Button>
      </div>

      <div className="min-h-0 flex-1">
        {tabs.map((tab) => (
          tab.kind === 'terminal' ? (
            <TerminalTabContent
              key={tab.id}
              sessionId={terminalSessionId}
              tabId={tab.id}
              isActive={tab.id === activeTab?.id}
            />
          ) : (
            <BrowserTabContent
              key={tab.id}
              tabId={tab.id}
              initialUrl={tab.url}
              isActive={tab.id === activeTab?.id}
              onMetadataChange={handleBrowserMetadataChange}
            />
          )
        ))}
      </div>
    </div>
  );
}
