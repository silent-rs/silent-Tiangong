import { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '@/api/tauri';
import { listen } from '@tauri-apps/api/event';
import { Globe, ArrowRight, ArrowLeft, RotateCw, CornerDownRight, Plus, X, PenTool, ScanSearch, ChevronDown, Clock, ExternalLink, History } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';

interface BrowserPanelProps {
  initialUrl?: string;
  currentUrl?: string;
}

interface TabInfo {
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
  entries: HistoryEntry[];
  currentIndex: number;
}

function normalizeBrowserUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim();
  if (!trimmed) return '';
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//i.test(trimmed)) return trimmed;
  if (/^about:/i.test(trimmed)) return trimmed;
  if (/^\//.test(trimmed)) return `file://${trimmed}`;
  return `https://${trimmed}`;
}

function formatTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 1) return '刚刚';
  if (diffMin < 60) return `${diffMin} 分钟前`;
  const diffHour = Math.floor(diffMin / 60);
  if (diffHour < 24) return `${diffHour} 小时前`;
  const diffDay = Math.floor(diffHour / 24);
  if (diffDay < 7) return `${diffDay} 天前`;
  return d.toLocaleDateString();
}

const DEFAULT_URL = 'about:blank';
const HISTORY_PAGE_SIZE = 20;

export function BrowserPanel({ initialUrl, currentUrl }: BrowserPanelProps) {
  const [url, setUrl] = useState(initialUrl || '');
  const [tabs, setTabs] = useState<TabInfo[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [annotationActive, setAnnotationActive] = useState(false);
  const [extractedElements, setExtractedElements] = useState<Array<{
    tag: string;
    text: string;
    selector: string;
    attributes: Record<string, string>;
  }> | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const initializedRef = useRef(false);
  const browserOpenedRef = useRef(false);

  const activeTabIdRef = useRef<string | null>(null);

  // 每个 tab 的浏览历史栈
  const [tabHistories, setTabHistories] = useState<Map<string, TabHistory>>(new Map());
  // 导航意图标记
  const navigationIntentRef = useRef<'new' | 'back' | 'forward' | null>(null);
  // 后退/前进下拉面板
  const [showBackHistory, setShowBackHistory] = useState(false);
  const [showForwardHistory, setShowForwardHistory] = useState(false);
  // 全局历史 Modal
  const [showHistoryModal, setShowHistoryModal] = useState(false);
  const [globalHistoryEntries, setGlobalHistoryEntries] = useState<HistoryEntry[]>([]);
  const [globalHistoryOffset, setGlobalHistoryOffset] = useState(0);
  const [globalHistoryHasMore, setGlobalHistoryHasMore] = useState(true);
  const [globalHistoryLoading, setGlobalHistoryLoading] = useState(false);

  const refreshTabs = useCallback(async () => {
    try {
      const result = await api.browserTabList();
      if (result.tabs.length === 0) {
        // 标签列表为空时创建一个空标签
        const tabId = await api.browserTabNew(DEFAULT_URL);
        activeTabIdRef.current = tabId;
        setActiveTabId(tabId);
        setUrl('');
        setTabs([{ id: tabId, url: DEFAULT_URL, title: '' }]);
      } else {
        setTabs(result.tabs);
        const activeId = result.active_tab_id || result.tabs[0].id;
        activeTabIdRef.current = activeId;
        setActiveTabId(activeId);
        browserOpenedRef.current = true;
      }
    } catch { /* ignore */ }
  }, []);

  // 获取当前 tab 的历史
  const getCurrentHistory = useCallback((tabId: string | null): TabHistory => {
    if (!tabId) return { entries: [], currentIndex: -1 };
    return tabHistories.get(tabId) ?? { entries: [], currentIndex: -1 };
  }, [tabHistories]);

  const canGoBack = useCallback((tabId: string | null): boolean => {
    const h = getCurrentHistory(tabId);
    return h.currentIndex > 0;
  }, [getCurrentHistory]);

  const canGoForward = useCallback((tabId: string | null): boolean => {
    const h = getCurrentHistory(tabId);
    return h.currentIndex >= 0 && h.currentIndex < h.entries.length - 1;
  }, [getCurrentHistory]);

  // 加载全局历史
  const loadGlobalHistory = useCallback(async (offset: number) => {
    setGlobalHistoryLoading(true);
    try {
      const entries = await api.browserGlobalHistory(offset, HISTORY_PAGE_SIZE);
      if (offset === 0) {
        setGlobalHistoryEntries(entries);
      } else {
        setGlobalHistoryEntries(prev => [...prev, ...entries]);
      }
      setGlobalHistoryHasMore(entries.length >= HISTORY_PAGE_SIZE);
      setGlobalHistoryOffset(offset + entries.length);
    } catch (err) {
      console.error('加载全局历史失败：', err);
      setGlobalHistoryHasMore(false);
    } finally {
      setGlobalHistoryLoading(false);
    }
  }, []);

  // 打开历史 Modal（先将 WebView 移到屏幕外，再显示 Modal）
  const openHistoryModal = useCallback(async () => {
    try {
      if (containerRef.current && browserOpenedRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        await api.browserSetPosition(-10000, -10000, rect.width, rect.height);
      }
    } catch { /* WebView 可能不存在，忽略 */ }
    setGlobalHistoryEntries([]);
    setGlobalHistoryOffset(0);
    setGlobalHistoryHasMore(true);
    loadGlobalHistory(0);
    setShowHistoryModal(true);
  }, [loadGlobalHistory]);

  useEffect(() => {
    if (currentUrl) {
      setUrl(currentUrl);
    }
  }, [currentUrl]);

  // 监听标签更新事件
  useEffect(() => {
    let unlistenTab: (() => void) | null = null;
    let unlistenPage: (() => void) | null = null;

    const setup = async () => {
      unlistenTab = await listen('browser:tab_updated', () => { refreshTabs(); });
      unlistenPage = await listen('browser:page_loaded', (event) => {
        refreshTabs();
        const payload = event.payload as { url?: string; title?: string };
        if (payload?.url) {
          setUrl(payload.url);
        }

        // 根据导航意图更新历史栈
        const intent = navigationIntentRef.current;
        navigationIntentRef.current = null;

        const pageUrl = payload?.url;
        const pageTitle = payload?.title ?? '';
        if (!pageUrl || pageUrl.startsWith('about:')) return;

        const tabId = activeTabIdRef.current;
        if (!tabId) return;

        setTabHistories(prev => {
          const next = new Map(prev);
          let history = next.get(tabId) ?? { entries: [], currentIndex: -1 };

          if (intent === 'back' || intent === 'forward') {
            // 后退/前进：查找匹配的 URL，移动索引
            const direction = intent === 'back' ? -1 : 1;
            const expectedIdx = history.currentIndex + direction;
            if (expectedIdx >= 0 && expectedIdx < history.entries.length && history.entries[expectedIdx].url === pageUrl) {
              history = { ...history, currentIndex: expectedIdx };
            } else {
              const foundIdx = history.entries.findIndex(e => e.url === pageUrl);
              if (foundIdx >= 0) {
                history = { ...history, currentIndex: foundIdx };
              } else {
                // 未找到匹配，按新导航处理
                const entries = [
                  ...history.entries.slice(0, history.currentIndex + 1),
                  { url: pageUrl, title: pageTitle, timestamp: Date.now() },
                ];
                history = { entries, currentIndex: entries.length - 1 };
              }
            }
          } else {
            // 新导航：截断并追加
            // 去重：URL 与最新条目相同时跳过
            if (history.entries.length > 0 && history.entries[history.currentIndex]?.url === pageUrl) {
              return prev;
            }
            const entries = [
              ...history.entries.slice(0, history.currentIndex + 1),
              { url: pageUrl, title: pageTitle, timestamp: Date.now() },
            ];
            history = { entries, currentIndex: entries.length - 1 };
          }

          next.set(tabId, history);
          return next;
        });
      });
    };
    setup();

    return () => {
      unlistenTab?.();
      unlistenPage?.();
    };
  }, [refreshTabs]);

  const syncPosition = useCallback(async () => {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height).catch(console.error);
  }, []);

  // 关闭 Modal 时恢复 WebView 位置
  useEffect(() => {
    if (!showHistoryModal && browserOpenedRef.current) {
      syncPosition();
    }
  }, [showHistoryModal, syncPosition]);

  const handleNavigate = useCallback(async () => {
    const nextUrl = normalizeBrowserUrl(url);
    if (!nextUrl) return;

    navigationIntentRef.current = 'new';

    try {
      await api.browserNavigate(nextUrl);
    } catch (err) {
      console.error('打开浏览器失败：', err);
    }
  }, [url]);

  const handleGoBack = useCallback(async () => {
    if (!activeTabId || !canGoBack(activeTabId)) return;
    navigationIntentRef.current = 'back';
    setShowBackHistory(false);
    setShowForwardHistory(false);
    await api.browserGoBack().catch(console.error);
  }, [activeTabId, canGoBack]);

  const handleGoForward = useCallback(async () => {
    if (!activeTabId || !canGoForward(activeTabId)) return;
    navigationIntentRef.current = 'forward';
    setShowBackHistory(false);
    setShowForwardHistory(false);
    await api.browserGoForward().catch(console.error);
  }, [activeTabId, canGoForward]);

  const handleReload = useCallback(async () => {
    await api.browserEval('location.reload()').catch(console.error);
  }, []);

  const handleToggleAnnotation = useCallback(async () => {
    try {
      if (annotationActive) {
        await api.browserEval('window.__tiangong_bridge.annotation.stop()');
        setAnnotationActive(false);
      } else {
        await api.browserEval('window.__tiangong_bridge.annotation.start("rect")');
        setAnnotationActive(true);
      }
    } catch (err) {
      console.error('批注切换失败：', err);
    }
  }, [annotationActive]);

  const handleAnnotationExtract = useCallback(async () => {
    try {
      const result = await api.browserAnnotationExtract();
      const allElements = result.elements.flatMap(r => r.elements);
      setExtractedElements(allElements);
    } catch (err) {
      console.error('批注元素提取失败：', err);
    }
  }, []);

  const handleTabNew = useCallback(async () => {
    try {
      const tabId = await api.browserTabNew(DEFAULT_URL);
      setActiveTabId(tabId);
      setUrl('');
      // 初始化空历史
      setTabHistories(prev => {
        const next = new Map(prev);
        next.set(tabId, { entries: [], currentIndex: -1 });
        return next;
      });
      await refreshTabs();
    } catch (err) {
      console.error('新建标签失败：', err);
    }
  }, [refreshTabs]);

  const handleTabSwitch = useCallback(async (tabId: string) => {
    try {
      navigationIntentRef.current = null;
      setShowBackHistory(false);
      setShowForwardHistory(false);
      await api.browserTabSwitch(tabId);
      activeTabIdRef.current = tabId;
      setActiveTabId(tabId);
      const tab = tabs.find(t => t.id === tabId);
      if (tab) {
        setUrl(tab.url);
      }
    } catch (err) {
      console.error('切换标签失败：', err);
    }
  }, [tabs]);

  const handleTabClose = useCallback(async (tabId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.browserTabClose(tabId);
      // 清除该 tab 的历史
      setTabHistories(prev => {
        const next = new Map(prev);
        next.delete(tabId);
        return next;
      });
      await refreshTabs();
    } catch (err) {
      console.error('关闭标签失败：', err);
    }
  }, [refreshTabs]);

  // 从历史记录跳转
  const handleHistoryJump = useCallback(async (targetUrl: string) => {
    navigationIntentRef.current = 'new';
    setShowBackHistory(false);
    setShowForwardHistory(false);
    setShowHistoryModal(false);
    try {
      if (!containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      if (browserOpenedRef.current) {
        await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height);
        await api.browserNavigate(targetUrl);
      } else {
        await api.browserOpen(targetUrl, rect.x, rect.y, rect.width, rect.height);
        browserOpenedRef.current = true;
      }
      setUrl(targetUrl);
    } catch (err) {
      console.error('历史跳转失败：', err);
    }
  }, []);

  // 关闭下拉菜单（点击外部）
  useEffect(() => {
    if (!showBackHistory && !showForwardHistory) return;
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest('.history-dropdown-container')) {
        setShowBackHistory(false);
        setShowForwardHistory(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [showBackHistory, showForwardHistory]);

  useEffect(() => {
    if (!containerRef.current) return;
    const observer = new ResizeObserver(() => {
      syncPosition();
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [syncPosition]);

  // 恢复浏览器面板时：重新检查 WebView 状态并同步位置
  useEffect(() => {
    const handleRestore = async () => {
      if (browserOpenedRef.current) {
        await syncPosition();
      } else if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        if (rect.width > 0 && rect.height > 0) {
          api.browserOpen(DEFAULT_URL, rect.x, rect.y, rect.width, rect.height)
            .then(() => { browserOpenedRef.current = true; })
            .then(() => refreshTabs())
            .catch(console.error);
        }
      }
      refreshTabs();
    };
    window.addEventListener('resize', syncPosition);
    window.addEventListener('tiangong:restore-browser-panel', handleRestore);
    return () => {
      window.removeEventListener('resize', syncPosition);
      window.removeEventListener('tiangong:restore-browser-panel', handleRestore);
    };
  }, [syncPosition, refreshTabs]);

  useEffect(() => {
    if (!initializedRef.current && containerRef.current) {
      initializedRef.current = true;
      let cancelled = false;
      let retries = 0;
      const tryOpen = () => {
        if (cancelled || !containerRef.current || retries > 10) return;
        const rect = containerRef.current.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) {
          retries++;
          requestAnimationFrame(tryOpen);
          return;
        }
        api.browserOpen(initialUrl || DEFAULT_URL, rect.x, rect.y, rect.width, rect.height)
          .then(() => { browserOpenedRef.current = true; })
          .then(() => new Promise<void>(resolve => requestAnimationFrame(() => resolve())))
          .then(() => {
            if (containerRef.current) {
              const r = containerRef.current.getBoundingClientRect();
              if (r.width > 0 && r.height > 0) {
                return api.browserSetPosition(r.x, r.y, r.width, r.height);
              }
            }
          })
          .then(() => refreshTabs())
          .catch(console.error);
      };
      requestAnimationFrame(tryOpen);
      return () => { cancelled = true; };
    }
  }, [initialUrl, refreshTabs]);

  const history = getCurrentHistory(activeTabId);
  const backEntries = history.entries.slice(0, history.currentIndex).reverse();
  const forwardEntries = history.entries.slice(history.currentIndex + 1);

  return (
    <div className="flex flex-1 flex-col h-full bg-background">
      {/* 标签栏 */}
      <div className="flex items-center gap-0.5 px-2 py-1 border-b shrink-0 overflow-x-auto">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`flex items-center gap-1 px-2 py-1 rounded text-xs cursor-pointer max-w-[150px] min-w-[80px] shrink-0 ${
              tab.id === activeTabId
                ? 'bg-muted text-foreground'
                : 'text-muted-foreground hover:bg-muted/50'
            }`}
            onClick={() => handleTabSwitch(tab.id)}
          >
            <span className="truncate flex-1">
              {tab.title || tab.url.replace(/^https?:\/\//, '').split('/')[0]}
            </span>
            <button
              className="shrink-0 hover:text-destructive"
              onClick={(e) => handleTabClose(tab.id, e)}
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        ))}
        <Button
          size="sm"
          variant="ghost"
          onClick={handleTabNew}
          className="h-6 w-6 p-0 shrink-0"
          title="新建标签"
        >
          <Plus className="w-3 h-3" />
        </Button>
      </div>

      {/* 工具栏 */}
      <div className="flex items-center gap-1 px-2 py-2 border-b shrink-0">
        {/* 后退按钮 + 下拉 */}
        <div className="relative history-dropdown-container">
          <div className="flex items-center">
            <Button
              size="sm"
              variant="ghost"
              onClick={handleGoBack}
              className="h-7 w-7 p-0 shrink-0"
              disabled={!canGoBack(activeTabId)}
            >
              <ArrowLeft className="w-3.5 h-3.5" />
            </Button>
            {backEntries.length > 0 && (
              <button
                className="h-7 w-3 flex items-center justify-center shrink-0 text-muted-foreground hover:text-foreground"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowBackHistory(!showBackHistory);
                  setShowForwardHistory(false);
                }}
              >
                <ChevronDown className="w-2.5 h-2.5" />
              </button>
            )}
          </div>
          {showBackHistory && backEntries.length > 0 && (
            <div className="absolute top-full left-0 mt-1 w-72 max-h-60 overflow-y-auto bg-popover border rounded-md shadow-lg z-50 text-xs">
              {backEntries.map((entry, i) => (
                <div
                  key={`${entry.url}-${entry.timestamp}-${i}`}
                  className="px-2 py-1.5 hover:bg-muted cursor-pointer border-b last:border-0"
                  onClick={() => {
                    handleHistoryJump(entry.url);
                    setShowBackHistory(false);
                  }}
                >
                  <div className="font-medium truncate">{entry.title || '(无标题)'}</div>
                  <div className="text-muted-foreground truncate">{entry.url}</div>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* 前进按钮 + 下拉 */}
        <div className="relative history-dropdown-container">
          <div className="flex items-center">
            <Button
              size="sm"
              variant="ghost"
              onClick={handleGoForward}
              className="h-7 w-7 p-0 shrink-0"
              disabled={!canGoForward(activeTabId)}
            >
              <ArrowRight className="w-3.5 h-3.5" />
            </Button>
            {forwardEntries.length > 0 && (
              <button
                className="h-7 w-3 flex items-center justify-center shrink-0 text-muted-foreground hover:text-foreground"
                onClick={(e) => {
                  e.stopPropagation();
                  setShowForwardHistory(!showForwardHistory);
                  setShowBackHistory(false);
                }}
              >
                <ChevronDown className="w-2.5 h-2.5" />
              </button>
            )}
          </div>
          {showForwardHistory && forwardEntries.length > 0 && (
            <div className="absolute top-full left-0 mt-1 w-72 max-h-60 overflow-y-auto bg-popover border rounded-md shadow-lg z-50 text-xs">
              {forwardEntries.map((entry, i) => (
                <div
                  key={`${entry.url}-${entry.timestamp}-${i}`}
                  className="px-2 py-1.5 hover:bg-muted cursor-pointer border-b last:border-0"
                  onClick={() => {
                    handleHistoryJump(entry.url);
                    setShowForwardHistory(false);
                  }}
                >
                  <div className="font-medium truncate">{entry.title || '(无标题)'}</div>
                  <div className="text-muted-foreground truncate">{entry.url}</div>
                </div>
              ))}
            </div>
          )}
        </div>

        <Button
          size="sm"
          variant="ghost"
          onClick={handleReload}
          className="h-7 w-7 p-0 shrink-0"
        >
          <RotateCw className="w-3.5 h-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={openHistoryModal}
          className="h-7 w-7 p-0 shrink-0"
          title="浏览历史"
        >
          <History className="w-3.5 h-3.5" />
        </Button>
        <Globe className="w-4 h-4 text-muted-foreground shrink-0 ml-1" />
        <Input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              handleNavigate();
            }
          }}
          placeholder="输入 URL..."
          className="flex-1 h-7 text-sm"
        />
        <Button
          size="sm"
          variant="ghost"
          onClick={handleNavigate}
          className="h-7 px-2 shrink-0"
          title="进入"
        >
          <CornerDownRight className="w-3.5 h-3.5 mr-1" />
        </Button>
        <Button
          size="sm"
          variant={annotationActive ? 'default' : 'ghost'}
          onClick={handleToggleAnnotation}
          className="h-7 w-7 p-0 shrink-0"
          title={annotationActive ? '关闭批注' : '开启批注'}
        >
          <PenTool className="w-3.5 h-3.5" />
        </Button>
        {annotationActive && (
          <Button
            size="sm"
            variant="ghost"
            onClick={handleAnnotationExtract}
            className="h-7 w-7 p-0 shrink-0"
            title="提取框选元素"
          >
            <ScanSearch className="w-3.5 h-3.5" />
          </Button>
        )}
      </div>

      {/* 批注元素提取结果 */}
      {extractedElements && extractedElements.length > 0 && (
        <div className="px-2 py-2 border-b bg-muted/20 text-xs max-h-40 overflow-y-auto">
          <div className="flex items-center justify-between mb-1">
            <span className="text-muted-foreground font-medium">提取到 {extractedElements.length} 个元素</span>
            <button
              className="text-muted-foreground hover:text-foreground"
              onClick={() => setExtractedElements(null)}
            >
              <X className="w-3 h-3" />
            </button>
          </div>
          {extractedElements.map((el, i) => (
            <div key={i} className="py-1 border-b border-border/30 last:border-0">
              <span className="text-primary font-mono">&lt;{el.tag}&gt;</span>
              {el.text && <span className="ml-1 truncate max-w-[60%] inline-block align-bottom">{el.text}</span>}
              <div className="text-muted-foreground font-mono text-[10px] truncate">{el.selector}</div>
            </div>
          ))}
        </div>
      )}

      {/* WebView 容器 */}
      <div className="relative flex-1">
        <div
          ref={containerRef}
          className="absolute inset-0 bg-muted/30"
        />
      </div>

      {/* 全局历史 Modal */}
      <Dialog open={showHistoryModal} onOpenChange={setShowHistoryModal}>
        <DialogContent className="max-w-xl p-0 overflow-hidden">
          <DialogHeader className="px-4 pt-4 pb-2">
            <DialogTitle className="flex items-center gap-2 text-base">
              <Clock className="w-4 h-4" />
              浏览历史
            </DialogTitle>
          </DialogHeader>
          <div
            className="px-4 pb-4 max-h-[60vh] overflow-y-auto"
            onScroll={(e) => {
              const el = e.currentTarget;
              if (el.scrollHeight - el.scrollTop - el.clientHeight < 100 && globalHistoryHasMore && !globalHistoryLoading) {
                loadGlobalHistory(globalHistoryOffset);
              }
            }}
          >
            {globalHistoryEntries.length === 0 && !globalHistoryLoading && (
              <div className="text-center py-12 text-muted-foreground text-sm">
                暂无浏览历史
              </div>
            )}
            {globalHistoryEntries.map((entry, i) => (
              <div
                key={`${entry.url}-${entry.timestamp}-${i}`}
                className="flex items-start gap-3 px-3 py-2.5 rounded-md hover:bg-muted/50 cursor-pointer group border-b border-border/20 last:border-0"
                onClick={() => handleHistoryJump(entry.url)}
              >
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium truncate group-hover:text-primary transition-colors">
                    {entry.title || entry.url}
                  </div>
                  <div className="flex items-center gap-2 mt-0.5">
                    <span className="text-xs text-muted-foreground truncate">
                      {entry.url.replace(/^https?:\/\//, '').split('/')[0]}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {formatTime(entry.timestamp)}
                    </span>
                  </div>
                </div>
                <ExternalLink className="w-3.5 h-3.5 text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity mt-0.5 shrink-0" />
              </div>
            ))}
            {globalHistoryLoading && (
              <div className="text-center py-4 text-muted-foreground text-sm">
                加载中...
              </div>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
