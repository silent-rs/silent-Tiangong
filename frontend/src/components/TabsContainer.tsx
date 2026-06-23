import { useCallback, useEffect, useMemo, useState } from 'react';
import { Globe, Plus, TerminalSquare, X } from 'lucide-react';
import type { TabKind, TabState } from '@/api/tauri';
import { useStore } from '@/store/useStore';
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

function createTab(kind: TabKind): TabState {
  const createdAt = nowText();
  return {
    id: newLocalTabId(kind),
    kind,
    title: kind === 'browser' ? '浏览器' : '终端',
    url: kind === 'browser' ? DEFAULT_BROWSER_URL : '',
    created_at: createdAt,
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
  const [tabs, setTabs] = useState<TabState[]>(() => [createTab(initialTabKind)]);
  const [activeTabId, setActiveTabId] = useState<string>(() => tabs[0]?.id ?? '');
  const terminalSessionId = activeSessionId || draftTerminalId || DRAFT_TERMINAL_ID;

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null,
    [activeTabId, tabs],
  );

  useEffect(() => {
    setTabs((currentTabs) => {
      if (currentTabs.some((tab) => tab.kind === initialTabKind)) {
        const existing = currentTabs.find((tab) => tab.kind === initialTabKind);
        if (existing) {
          setActiveTabId(existing.id);
        }
        return currentTabs;
      }
      const nextTab = createTab(initialTabKind);
      setActiveTabId(nextTab.id);
      return [...currentTabs, nextTab];
    });
  }, [initialTabKind]);

  const handleNewTab = useCallback((kind: TabKind) => {
    setTabs((currentTabs) => {
      const nextTab = createTab(kind);
      setActiveTabId(nextTab.id);
      return [...currentTabs, nextTab];
    });
  }, []);

  const handleSwitchTab = useCallback((tabId: string) => {
    setActiveTabId(tabId);
  }, []);

  const handleCloseTab = useCallback((tabId: string) => {
    setTabs((currentTabs) => {
      const closedIndex = currentTabs.findIndex((tab) => tab.id === tabId);
      if (closedIndex === -1) return currentTabs;
      const nextTabs = currentTabs.filter((tab) => tab.id !== tabId);
      if (nextTabs.length === 0) {
        setActiveTabId('');
        onClose();
        return [];
      }
      if (activeTabId === tabId) {
        setActiveTabId(pickNextActiveTab(nextTabs, closedIndex) ?? '');
      }
      return nextTabs;
    });
  }, [activeTabId, onClose]);

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
            <div
              key={tab.id}
              className={`h-full items-center justify-center text-sm text-muted-foreground ${
                tab.id === activeTab?.id ? 'flex' : 'hidden'
              }`}
            >
              浏览器 Tab 内容将在后续任务接入
            </div>
          )
        ))}
      </div>
    </div>
  );
}
