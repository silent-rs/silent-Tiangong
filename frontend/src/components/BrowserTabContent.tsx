import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '@/api/tauri';
import { listen } from '@tauri-apps/api/event';
import {
  ArrowLeft,
  ArrowRight,
  Clock,
  CornerDownRight,
  Globe,
  History,
  PenTool,
  RotateCw,
  ScanSearch,
  Trash2,
  X,
  ZoomIn,
  ZoomOut,
} from 'lucide-react';
import { Button } from './ui/button';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Input } from './ui/input';

interface BrowserTabContentProps {
  tabId: string;
  initialUrl?: string;
  isActive: boolean;
  onMetadataChange?: (tabId: string, metadata: { title?: string; url?: string }) => void;
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

interface BrowserPageEvent {
  url?: string;
  title?: string;
  text?: string;
}

const DEFAULT_URL = 'about:blank';
const HISTORY_PAGE_SIZE = 20;

function normalizeBrowserUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim();
  if (!trimmed) return '';
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//i.test(trimmed)) return trimmed;
  if (/^about:/i.test(trimmed)) return trimmed;
  if (/^\//.test(trimmed)) return `file://${trimmed}`;
  return `https://${trimmed}`;
}

function isBlankBrowserUrl(url: string): boolean {
  return !url || url === DEFAULT_URL;
}

function displayUrl(url: string): string {
  return isBlankBrowserUrl(url) ? '' : url;
}

function fallbackTitle(url: string): string {
  if (isBlankBrowserUrl(url)) return '浏览器';
  return url.replace(/^https?:\/\//, '').split('/')[0] || '浏览器';
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

export function BrowserTabContent({
  tabId,
  initialUrl,
  isActive,
  onMetadataChange,
}: BrowserTabContentProps) {
  const [url, setUrl] = useState(displayUrl(initialUrl || DEFAULT_URL));
  const [annotationActive, setAnnotationActive] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [history, setHistory] = useState<TabHistory>({ entries: [], currentIndex: -1 });
  const [extractedElements, setExtractedElements] = useState<Array<{
    tag: string;
    text: string;
    selector: string;
    attributes: Record<string, string>;
  }> | null>(null);
  const [showHistoryModal, setShowHistoryModal] = useState(false);
  const [globalHistoryEntries, setGlobalHistoryEntries] = useState<HistoryEntry[]>([]);
  const [globalHistoryOffset, setGlobalHistoryOffset] = useState(0);
  const [globalHistoryHasMore, setGlobalHistoryHasMore] = useState(true);
  const [globalHistoryLoading, setGlobalHistoryLoading] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const isActiveRef = useRef(isActive);
  const initializedRealUrlRef = useRef<string | null>(null);

  useEffect(() => {
    isActiveRef.current = isActive;
  }, [isActive]);

  const publishMetadata = useCallback((nextUrl?: string, nextTitle?: string) => {
    if (!onMetadataChange) return;
    onMetadataChange(tabId, {
      url: nextUrl,
      title: nextTitle || (nextUrl ? fallbackTitle(nextUrl) : undefined),
    });
  }, [onMetadataChange, tabId]);

  const syncPosition = useCallback(async () => {
    if (!isActiveRef.current || !containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height).catch(console.error);
  }, []);

  const refreshTabHistory = useCallback(async () => {
    try {
      const result = await api.browserTabHistory(tabId);
      setHistory({
        entries: result.entries,
        currentIndex: result.current_index,
      });
    } catch {
      setHistory({ entries: [], currentIndex: -1 });
    }
  }, [tabId]);

  const refreshBackendTabState = useCallback(async () => {
    try {
      const result = await api.browserTabList();
      const tab = result.tabs.find((item) => item.id === tabId);
      if (!tab) return;
      setUrl(displayUrl(tab.url));
      publishMetadata(tab.url, tab.title);
      if (result.active_tab_id === tabId) {
        await refreshTabHistory();
        await syncPosition();
      }
    } catch {
      // 浏览器运行时可能尚未初始化，忽略即可。
    }
  }, [publishMetadata, refreshTabHistory, syncPosition, tabId]);

  const activateBackendTab = useCallback(async () => {
    if (!isActiveRef.current) return;
    await api.browserTabSwitch(tabId);
    await syncPosition();
    await refreshBackendTabState();
  }, [refreshBackendTabState, syncPosition, tabId]);

  const navigateToUrl = useCallback(async (rawUrl: string) => {
    const nextUrl = normalizeBrowserUrl(rawUrl);
    if (!nextUrl) return;

    try {
      await api.browserTabSwitch(tabId);
      await syncPosition();

      if (isBlankBrowserUrl(nextUrl)) {
        setUrl('');
        publishMetadata(DEFAULT_URL, '浏览器');
        return;
      }

      await api.browserNavigate(nextUrl);
      setUrl(nextUrl);
      publishMetadata(nextUrl);
      await refreshTabHistory();
    } catch (err) {
      console.error('打开浏览器失败：', err);
    }
  }, [publishMetadata, refreshTabHistory, syncPosition, tabId]);

  const handleNavigate = useCallback(() => {
    void navigateToUrl(url);
  }, [navigateToUrl, url]);

  const canGoBack = history.currentIndex > 0;
  const canGoForward = history.currentIndex >= 0 && history.currentIndex < history.entries.length - 1;

  const handleGoBack = useCallback(async () => {
    if (!canGoBack) return;
    try {
      await api.browserTabSwitch(tabId);
      await api.browserGoBack();
    } catch (err) {
      console.error('后退失败：', err);
    }
  }, [canGoBack, tabId]);

  const handleGoForward = useCallback(async () => {
    if (!canGoForward) return;
    try {
      await api.browserTabSwitch(tabId);
      await api.browserGoForward();
    } catch (err) {
      console.error('前进失败：', err);
    }
  }, [canGoForward, tabId]);

  const handleReload = useCallback(async () => {
    if (!url) return;
    await api.browserEval('location.reload()').catch(console.error);
  }, [url]);

  const dismissAnnotationBeforeZoom = useCallback(async () => {
    if (!annotationActive) return;
    try {
      await api.browserEval('window.__tiangong_bridge && window.__tiangong_bridge.annotation && window.__tiangong_bridge.annotation.stop();');
    } catch (err) {
      console.error('关闭批注失败：', err);
    }
    setAnnotationActive(false);
  }, [annotationActive]);

  const handleZoomIn = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserSetZoom(+(zoom + 0.1).toFixed(2));
      setZoom(next);
    } catch (err) {
      console.error('放大失败：', err);
    }
  }, [dismissAnnotationBeforeZoom, zoom]);

  const handleZoomOut = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserSetZoom(+(zoom - 0.1).toFixed(2));
      setZoom(next);
    } catch (err) {
      console.error('缩小失败：', err);
    }
  }, [dismissAnnotationBeforeZoom, zoom]);

  const handleZoomReset = useCallback(async () => {
    await dismissAnnotationBeforeZoom();
    try {
      const next = await api.browserResetZoom();
      setZoom(next);
    } catch (err) {
      console.error('重置缩放失败：', err);
    }
  }, [dismissAnnotationBeforeZoom]);

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
      const allElements = result.elements.flatMap((entry) => entry.elements);
      setExtractedElements(allElements);
    } catch (err) {
      console.error('批注元素提取失败：', err);
    }
  }, []);

  const loadGlobalHistory = useCallback(async (offset: number) => {
    setGlobalHistoryLoading(true);
    try {
      const entries = await api.browserGlobalHistory(offset, HISTORY_PAGE_SIZE);
      if (offset === 0) {
        setGlobalHistoryEntries(entries);
      } else {
        setGlobalHistoryEntries((prev) => [...prev, ...entries]);
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

  const openHistoryModal = useCallback(async () => {
    try {
      if (containerRef.current) {
        const rect = containerRef.current.getBoundingClientRect();
        await api.browserSetPosition(-10000, -10000, rect.width, rect.height);
      }
    } catch {
      // WebView 可能尚未创建。
    }
    setGlobalHistoryEntries([]);
    setGlobalHistoryOffset(0);
    setGlobalHistoryHasMore(true);
    void loadGlobalHistory(0);
    setShowHistoryModal(true);
  }, [loadGlobalHistory]);

  const handleHistoryJump = useCallback(async (targetUrl: string) => {
    setShowHistoryModal(false);
    setUrl(targetUrl);
    await navigateToUrl(targetUrl);
  }, [navigateToUrl]);

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

  const handleDeleteHistoryEntry = useCallback(async (entryUrl: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.browserGlobalHistoryDelete(entryUrl);
      setGlobalHistoryEntries((prev) => prev.filter((entry) => entry.url !== entryUrl));
    } catch (err) {
      console.error('删除历史条目失败：', err);
    }
  }, []);

  useEffect(() => {
    setUrl(displayUrl(initialUrl || DEFAULT_URL));
  }, [initialUrl]);

  useEffect(() => {
    api.browserGetZoom().then(setZoom).catch((err) => {
      console.error('读取缩放失败：', err);
    });
  }, []);

  useEffect(() => {
    if (!isActive) {
      if (annotationActive) {
        setAnnotationActive(false);
      }
      return;
    }

    let cancelled = false;
    const run = async () => {
      try {
        await activateBackendTab();
        if (cancelled) return;
        const normalizedInitialUrl = normalizeBrowserUrl(initialUrl || '');
        if (!isBlankBrowserUrl(normalizedInitialUrl) && initializedRealUrlRef.current !== normalizedInitialUrl) {
          initializedRealUrlRef.current = normalizedInitialUrl;
          await navigateToUrl(normalizedInitialUrl);
        }
      } catch (err) {
        console.error('切换浏览器标签失败：', err);
      }
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [activateBackendTab, annotationActive, initialUrl, isActive, navigateToUrl]);

  useEffect(() => {
    if (!showHistoryModal && isActive) {
      void syncPosition();
    }
  }, [isActive, showHistoryModal, syncPosition]);

  useEffect(() => {
    if (!isActive) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const key = e.key.toLowerCase();
      if (key === '=' || key === '+') {
        e.preventDefault();
        void handleZoomIn();
      } else if (key === '-') {
        e.preventDefault();
        void handleZoomOut();
      } else if (key === '0') {
        e.preventDefault();
        void handleZoomReset();
      }
    };
    window.addEventListener('keydown', handleKeyDown, true);
    return () => window.removeEventListener('keydown', handleKeyDown, true);
  }, [handleZoomIn, handleZoomOut, handleZoomReset, isActive]);

  useEffect(() => {
    const observer = new ResizeObserver(() => {
      if (isActiveRef.current) {
        void syncPosition();
      }
    });
    if (containerRef.current) {
      observer.observe(containerRef.current);
    }
    return () => observer.disconnect();
  }, [syncPosition]);

  useEffect(() => {
    if (!isActive) return;

    const handleRestore = () => {
      void syncPosition();
      void refreshBackendTabState();
    };

    const handleFocus = () => {
      void syncPosition();
    };

    let unlistenRestore: (() => void) | null = null;
    listen('browser:restore', () => {
      if (isActiveRef.current) {
        void syncPosition();
      }
    }).then((fn) => {
      unlistenRestore = fn;
    }).catch(() => {});

    window.addEventListener('resize', handleRestore);
    window.addEventListener('tiangong:restore-browser-panel', handleRestore);
    window.addEventListener('focus', handleFocus);

    return () => {
      unlistenRestore?.();
      window.removeEventListener('resize', handleRestore);
      window.removeEventListener('tiangong:restore-browser-panel', handleRestore);
      window.removeEventListener('focus', handleFocus);
    };
  }, [isActive, refreshBackendTabState, syncPosition]);

  useEffect(() => {
    let unlistenTab: (() => void) | null = null;
    let unlistenPage: (() => void) | null = null;
    let cancelled = false;

    const setup = async () => {
      unlistenTab = await listen('browser:tab_updated', () => {
        if (cancelled || !isActiveRef.current) return;
        void refreshBackendTabState();
      });
      unlistenPage = await listen('browser:page_loaded', (event) => {
        if (cancelled || !isActiveRef.current) return;
        const payload = event.payload as BrowserPageEvent;
        if (payload?.url) {
          setUrl(displayUrl(payload.url));
          publishMetadata(payload.url, payload.title);
        }
        void refreshTabHistory();
      });
    };

    setup().catch(() => {});
    return () => {
      cancelled = true;
      unlistenTab?.();
      unlistenPage?.();
    };
  }, [publishMetadata, refreshBackendTabState, refreshTabHistory]);

  return (
    <div className={`h-full flex-col bg-background ${isActive ? 'flex' : 'hidden'}`}>
      <div className="flex shrink-0 items-center gap-1 border-b px-2 py-2">
        <Button
          size="sm"
          variant="ghost"
          onClick={handleGoBack}
          className="h-7 w-7 shrink-0 p-0"
          disabled={!canGoBack}
          title="后退"
        >
          <ArrowLeft className="h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleGoForward}
          className="h-7 w-7 shrink-0 p-0"
          disabled={!canGoForward}
          title="前进"
        >
          <ArrowRight className="h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleReload}
          className="h-7 w-7 shrink-0 p-0"
          disabled={!url}
          title="刷新"
        >
          <RotateCw className="h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleZoomOut}
          className="h-7 w-7 shrink-0 p-0"
          title="缩小 (Cmd/Ctrl -)"
          disabled={zoom <= 0.25 + 1e-6}
        >
          <ZoomOut className="h-3.5 w-3.5" />
        </Button>
        <button
          type="button"
          onDoubleClick={handleZoomReset}
          className="h-7 shrink-0 px-1.5 text-xs tabular-nums text-muted-foreground transition-colors hover:text-foreground"
          title="双击重置为 100% (Cmd/Ctrl 0)"
        >
          {Math.round(zoom * 100)}%
        </button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleZoomIn}
          className="h-7 w-7 shrink-0 p-0"
          title="放大 (Cmd/Ctrl +)"
          disabled={zoom >= 5.0 - 1e-6}
        >
          <ZoomIn className="h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={openHistoryModal}
          className="h-7 w-7 shrink-0 p-0"
          title="浏览历史"
        >
          <History className="h-3.5 w-3.5" />
        </Button>
        <Globe className="ml-1 h-4 w-4 shrink-0 text-muted-foreground" />
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
          className="h-7 flex-1 text-sm"
        />
        <Button
          size="sm"
          variant="ghost"
          onClick={handleNavigate}
          className="h-7 shrink-0 px-2"
          title="进入"
        >
          <CornerDownRight className="mr-1 h-3.5 w-3.5" />
        </Button>
        <Button
          size="sm"
          variant={annotationActive ? 'default' : 'ghost'}
          onClick={handleToggleAnnotation}
          className="h-7 w-7 shrink-0 p-0"
          title={annotationActive ? '关闭批注' : '开启批注'}
          disabled={!url}
        >
          <PenTool className="h-3.5 w-3.5" />
        </Button>
        {annotationActive && (
          <Button
            size="sm"
            variant="ghost"
            onClick={handleAnnotationExtract}
            className="h-7 w-7 shrink-0 p-0"
            title="提取框选元素"
          >
            <ScanSearch className="h-3.5 w-3.5" />
          </Button>
        )}
      </div>

      {extractedElements && extractedElements.length > 0 && (
        <div className="max-h-40 overflow-y-auto border-b bg-muted/20 px-2 py-2 text-xs">
          <div className="mb-1 flex items-center justify-between">
            <span className="font-medium text-muted-foreground">提取到 {extractedElements.length} 个元素</span>
            <button
              className="text-muted-foreground hover:text-foreground"
              onClick={() => setExtractedElements(null)}
              title="关闭"
            >
              <X className="h-3 w-3" />
            </button>
          </div>
          {extractedElements.map((el, i) => (
            <div key={`${el.selector}-${i}`} className="border-b border-border/30 py-1 last:border-0">
              <span className="font-mono text-primary">&lt;{el.tag}&gt;</span>
              {el.text && <span className="ml-1 inline-block max-w-[60%] truncate align-bottom">{el.text}</span>}
              <div className="truncate font-mono text-[10px] text-muted-foreground">{el.selector}</div>
            </div>
          ))}
        </div>
      )}

      <div className="relative flex-1">
        <div
          ref={containerRef}
          className="absolute inset-0 bg-muted/30"
        />
      </div>

      <Dialog open={showHistoryModal} onOpenChange={setShowHistoryModal}>
        <DialogContent className="max-w-xl overflow-hidden p-0">
          <DialogHeader className="px-4 pb-2 pt-4">
            <DialogTitle className="flex items-center gap-2 text-base">
              <Clock className="h-4 w-4" />
              浏览历史
            </DialogTitle>
          </DialogHeader>
          <div
            className="max-h-[60vh] overflow-y-auto px-4"
            onScroll={(e) => {
              const el = e.currentTarget;
              if (el.scrollHeight - el.scrollTop - el.clientHeight < 100 && globalHistoryHasMore && !globalHistoryLoading) {
                void loadGlobalHistory(globalHistoryOffset);
              }
            }}
          >
            {globalHistoryEntries.length === 0 && !globalHistoryLoading && (
              <div className="py-12 text-center text-sm text-muted-foreground">
                暂无浏览历史
              </div>
            )}
            {globalHistoryEntries.map((entry, i) => (
              <div
                key={`${entry.url}-${entry.timestamp}-${i}`}
                className="group flex cursor-pointer items-start gap-3 rounded-md border-b border-border/20 px-3 py-2.5 last:border-0 hover:bg-muted/50"
                onClick={() => { void handleHistoryJump(entry.url); }}
              >
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium transition-colors group-hover:text-primary">
                    {entry.title || entry.url}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2">
                    <span className="truncate text-xs text-muted-foreground">
                      {entry.url.replace(/^https?:\/\//, '').split('/')[0]}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {formatTime(entry.timestamp)}
                    </span>
                  </div>
                </div>
                <button
                  className="mt-0.5 shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 hover:text-destructive"
                  onClick={(e) => { void handleDeleteHistoryEntry(entry.url, e); }}
                  title="删除"
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
            ))}
            {globalHistoryLoading && (
              <div className="py-4 text-center text-sm text-muted-foreground">
                加载中...
              </div>
            )}
          </div>
          {globalHistoryEntries.length > 0 && (
            <div className="border-t px-4 py-3">
              <Button
                size="sm"
                onClick={handleClearAllHistory}
                className="w-full bg-destructive text-white hover:bg-destructive/90"
              >
                <Trash2 className="mr-1.5 h-3.5 w-3.5" />
                清空全部历史
              </Button>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}
