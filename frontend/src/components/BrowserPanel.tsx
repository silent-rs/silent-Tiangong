import { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '@/api/tauri';
import { listen } from '@tauri-apps/api/event';
import { Globe, ArrowRight, ArrowLeft, RotateCw, CornerDownRight, Plus, X, PenTool, ScanSearch, Clock, History, Trash2, ZoomIn, ZoomOut } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';

interface BrowserPanelProps {
  sessionId?: string;
  initialUrl?: string;
  currentUrl?: string;
  onClose?: () => void;
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

function isBlankBrowserUrl(url: string): boolean {
  return !url || url === DEFAULT_URL;
}

export function BrowserPanel({ sessionId = '', initialUrl, currentUrl, onClose }: BrowserPanelProps) {
  const [url, setUrl] = useState(initialUrl || '');
  const [tabs, setTabs] = useState<TabInfo[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [annotationActive, setAnnotationActive] = useState(false);
  const [zoom, setZoom] = useState(1);
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
  // 全局历史 Modal
  const [showHistoryModal, setShowHistoryModal] = useState(false);
  const [globalHistoryEntries, setGlobalHistoryEntries] = useState<HistoryEntry[]>([]);
  const [globalHistoryOffset, setGlobalHistoryOffset] = useState(0);
  const [globalHistoryHasMore, setGlobalHistoryHasMore] = useState(true);
  const [globalHistoryLoading, setGlobalHistoryLoading] = useState(false);

  const refreshTabs = useCallback(async () => {
    try {
      const result = await api.browserTabList(sessionId, );
      if (result.tabs.length === 0) {
        // 所有 tab 已关闭，关闭浏览器面板
        browserOpenedRef.current = false;
        setTabs([]);
        setActiveTabId(null);
        activeTabIdRef.current = null;
        setUrl('');
        onClose?.();
      } else {
        setTabs(result.tabs);
        const activeId = result.active_tab_id || result.tabs[0].id;
        activeTabIdRef.current = activeId;
        setActiveTabId(activeId);
        browserOpenedRef.current = true;
      }
    } catch { /* ignore */ }
  }, [onClose]);

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

  // 清空全部全局历史
  const handleClearAllHistory = useCallback(async () => {
    try {
      await api.browserGlobalHistoryClear();
      setGlobalHistoryEntries([]);
      setGlobalHistoryOffset(0);
      setGlobalHistoryHasMore(false);
    } catch (err) {
      console.error('清空历史失败：', err);
    }
  }, []);

  // 删除单条全局历史
  const handleDeleteHistoryEntry = useCallback(async (url: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.browserGlobalHistoryDelete(url);
      setGlobalHistoryEntries(prev => prev.filter(entry => entry.url !== url));
    } catch (err) {
      console.error('删除历史条目失败：', err);
    }
  }, []);

  // 打开历史 Modal（先将 WebView 移到屏幕外，再显示 Modal）
  const openHistoryModal = useCallback(async () => {
    try {
      if (containerRef.current && browserOpenedRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        await api.browserSetPosition(sessionId, -10000, -10000, rect.width, rect.height);
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
      unlistenTab = await listen('browser:tab_updated', () => {
        refreshTabs();
        // WebView 可能刚被创建（about:blank 标签延迟创建），需要同步位置到容器
        syncPosition();
      });
      unlistenPage = await listen('browser:page_loaded', (event) => {
        const payload = event.payload as {
          session_id?: string;
          tab_id?: string;
          url?: string;
          title?: string;
        };
        if (payload.session_id !== sessionId) return;
        if (payload.tab_id && payload.tab_id !== activeTabIdRef.current) return;
        refreshTabs();
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
    await api.browserSetPosition(sessionId, rect.x, rect.y, rect.width, rect.height).catch(console.error);
  }, []);

  const ensureBlankTabMetadata = useCallback(async (rect: DOMRect) => {
    await api.browserSetPosition(sessionId, rect.x, rect.y, rect.width, rect.height);
    const result = await api.browserTabList(sessionId, );
    if (result.tabs.length === 0) {
      const tabId = await api.browserTabNew(sessionId, DEFAULT_URL);
      activeTabIdRef.current = tabId;
      setActiveTabId(tabId);
      setUrl('');
      setTabHistories(prev => {
        const next = new Map(prev);
        next.set(tabId, { entries: [], currentIndex: -1 });
        return next;
      });
    }
    browserOpenedRef.current = true;
    await refreshTabs();
  }, [refreshTabs]);

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
      if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        await api.browserSetPosition(sessionId, rect.x, rect.y, rect.width, rect.height);
      }
      await api.browserNavigate(sessionId, nextUrl);
      browserOpenedRef.current = true;
    } catch (err) {
      console.error('打开浏览器失败：', err);
    }
  }, [url]);

  const handleGoBack = useCallback(async () => {
    if (!activeTabId || !canGoBack(activeTabId)) return;
    navigationIntentRef.current = 'back';
    await api.browserGoBack(sessionId, ).catch(console.error);
  }, [activeTabId, canGoBack]);

  const handleGoForward = useCallback(async () => {
    if (!activeTabId || !canGoForward(activeTabId)) return;
    navigationIntentRef.current = 'forward';
    await api.browserGoForward(sessionId, ).catch(console.error);
  }, [activeTabId, canGoForward]);

  const handleReload = useCallback(async () => {
    const nextUrl = normalizeBrowserUrl(url);
    if (!nextUrl) return;
    await api.browserNavigate(sessionId, nextUrl).catch(console.error);
  }, [sessionId, url]);

  // 缩放前关闭批注：批注 canvas 是 webview 内 DOM，set_zoom 会等比缩放整个 webview，
  // 因此缩放前若批注处于激活状态则先关闭并清空，避免视觉错位；用户可在缩放后重新开启。
  const dismissAnnotationBeforeZoom = useCallback(async () => {
    if (!annotationActive) return;
    try {
      await api.browserEval(sessionId, 'window.__tiangong_bridge && window.__tiangong_bridge.annotation && window.__tiangong_bridge.annotation.stop();');
    } catch (err) {
      console.error('关闭批注失败：', err);
    }
    setAnnotationActive(false);
  }, [annotationActive]);

  const handleZoomIn = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserSetZoom(sessionId, +(zoom + 0.1).toFixed(2));
      setZoom(next);
    } catch (err) {
      console.error('放大失败：', err);
    }
  }, [zoom, dismissAnnotationBeforeZoom]);

  const handleZoomOut = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserSetZoom(sessionId, +(zoom - 0.1).toFixed(2));
      setZoom(next);
    } catch (err) {
      console.error('缩小失败：', err);
    }
  }, [zoom, dismissAnnotationBeforeZoom]);

  const handleZoomReset = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserResetZoom(sessionId, );
      setZoom(next);
    } catch (err) {
      console.error('重置缩放失败：', err);
    }
  }, [dismissAnnotationBeforeZoom]);

  // 初始化：读取持久化的缩放比例
  useEffect(() => {
    api.browserGetZoom(sessionId, ).then(setZoom).catch((err) => {
      console.error('读取缩放失败：', err);
    });
  }, []);

  // 快捷键：Cmd/Ctrl +/-/0（仅在浏览器面板挂载时注册）
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const key = e.key.toLowerCase();
      if (key === '=' || key === '+') {
        e.preventDefault();
        handleZoomIn();
      } else if (key === '-') {
        e.preventDefault();
        handleZoomOut();
      } else if (key === '0') {
        e.preventDefault();
        handleZoomReset();
      }
    };
    window.addEventListener('keydown', handleKeyDown, true);
    return () => window.removeEventListener('keydown', handleKeyDown, true);
  }, [handleZoomIn, handleZoomOut, handleZoomReset]);

  const handleToggleAnnotation = useCallback(async () => {
    try {
      if (annotationActive) {
        await api.browserEval(sessionId, 'window.__tiangong_bridge.annotation.stop()');
        setAnnotationActive(false);
      } else {
        await api.browserEval(sessionId, 'window.__tiangong_bridge.annotation.start("rect")');
        setAnnotationActive(true);
      }
    } catch (err) {
      console.error('批注切换失败：', err);
    }
  }, [annotationActive]);

  const handleAnnotationExtract = useCallback(async () => {
    try {
      const result = await api.browserAnnotationExtract(sessionId, );
      const allElements = result.elements.flatMap(r => r.elements);
      setExtractedElements(allElements);
    } catch (err) {
      console.error('批注元素提取失败：', err);
    }
  }, []);

  const handleTabNew = useCallback(async () => {
    try {
      const tabId = await api.browserTabNew(sessionId, DEFAULT_URL);
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
      await api.browserTabSwitch(sessionId, tabId);
      activeTabIdRef.current = tabId;
      setActiveTabId(tabId);
      const tab = tabs.find(t => t.id === tabId);
      if (tab) {
        setUrl(tab.url === DEFAULT_URL ? '' : tab.url);
      }
    } catch (err) {
      console.error('切换标签失败：', err);
    }
  }, [tabs]);

  const handleTabClose = useCallback(async (tabId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.browserTabClose(sessionId, tabId);
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

  // 从历史记录跳转（全局历史 Modal 使用：作为新导航）
  const handleHistoryJump = useCallback(async (targetUrl: string) => {
    navigationIntentRef.current = 'new';
    setShowHistoryModal(false);
    try {
      if (!containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      if (browserOpenedRef.current) {
        await api.browserSetPosition(sessionId, rect.x, rect.y, rect.width, rect.height);
        await api.browserNavigate(sessionId, targetUrl);
      } else {
        await api.browserOpen(sessionId, targetUrl, rect.x, rect.y, rect.width, rect.height);
        browserOpenedRef.current = true;
      }
      setUrl(targetUrl);
    } catch (err) {
      console.error('历史跳转失败：', err);
    }
  }, []);

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
          await ensureBlankTabMetadata(rect);
          return;
        }
      }
      refreshTabs();
    };
    // 窗口获得焦点时同步 WebView 位置
    const handleFocus = () => {
      if (browserOpenedRef.current) {
        syncPosition();
      }
    };

    let unlistenRestore: (() => void) | null = null;
    listen('browser:restore', () => {
      if (browserOpenedRef.current) {
        syncPosition();
      }
    }).then(fn => { unlistenRestore = fn; });

    window.addEventListener('resize', syncPosition);
    window.addEventListener('tiangong:restore-browser-panel', handleRestore);
    window.addEventListener('focus', handleFocus);
    return () => {
      unlistenRestore?.();
      window.removeEventListener('resize', syncPosition);
      window.removeEventListener('tiangong:restore-browser-panel', handleRestore);
      window.removeEventListener('focus', handleFocus);
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
        const normalizedInitialUrl = normalizeBrowserUrl(initialUrl || '');
        const openPromise = isBlankBrowserUrl(normalizedInitialUrl)
          ? ensureBlankTabMetadata(rect)
          : api.browserOpen(sessionId, normalizedInitialUrl, rect.x, rect.y, rect.width, rect.height)
            .then(() => {
              browserOpenedRef.current = true;
              setUrl(normalizedInitialUrl);
            });
        openPromise
          .then(() => new Promise<void>(resolve => requestAnimationFrame(() => resolve())))
          .then(() => {
            if (containerRef.current) {
              const r = containerRef.current.getBoundingClientRect();
              if (r.width > 0 && r.height > 0) {
                return api.browserSetPosition(sessionId, r.x, r.y, r.width, r.height);
              }
            }
          })
          .then(() => refreshTabs())
          .catch(console.error);
      };
      requestAnimationFrame(tryOpen);
      return () => { cancelled = true; };
    }
  }, [ensureBlankTabMetadata, initialUrl, refreshTabs]);

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
        <Button
          size="sm"
          variant="ghost"
          onClick={handleGoBack}
          className="h-7 w-7 p-0 shrink-0"
          disabled={!canGoBack(activeTabId)}
        >
          <ArrowLeft className="w-3.5 h-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleGoForward}
          className="h-7 w-7 p-0 shrink-0"
          disabled={!canGoForward(activeTabId)}
        >
          <ArrowRight className="w-3.5 h-3.5" />
        </Button>

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
          onClick={handleZoomOut}
          className="h-7 w-7 p-0 shrink-0"
          title="缩小 (Cmd/Ctrl -)"
          disabled={zoom <= 0.25 + 1e-6}
        >
          <ZoomOut className="w-3.5 h-3.5" />
        </Button>
        <button
          type="button"
          onDoubleClick={handleZoomReset}
          className="h-7 px-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors shrink-0 tabular-nums"
          title="双击重置为 100% (Cmd/Ctrl 0)"
        >
          {Math.round(zoom * 100)}%
        </button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleZoomIn}
          className="h-7 w-7 p-0 shrink-0"
          title="放大 (Cmd/Ctrl +)"
          disabled={zoom >= 5.0 - 1e-6}
        >
          <ZoomIn className="w-3.5 h-3.5" />
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
            className="px-4 max-h-[60vh] overflow-y-auto"
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
                <button
                  className="shrink-0 mt-0.5 p-1 rounded opacity-0 group-hover:opacity-100 text-muted-foreground hover:text-destructive transition-opacity"
                  onClick={(e) => handleDeleteHistoryEntry(entry.url, e)}
                  title="删除"
                >
                  <X className="w-3 h-3" />
                </button>
              </div>
            ))}
            {globalHistoryLoading && (
              <div className="text-center py-4 text-muted-foreground text-sm">
                加载中...
              </div>
            )}
          </div>
          {globalHistoryEntries.length > 0 && (
            <div className="px-4 py-3 border-t">
              <Button
                size="sm"
                onClick={handleClearAllHistory}
                className="w-full bg-destructive text-white hover:bg-destructive/90"
              >
                <Trash2 className="w-3.5 h-3.5 mr-1.5" />
                清空全部历史
              </Button>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}
