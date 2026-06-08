import { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '@/api/tauri';
import { listen } from '@tauri-apps/api/event';
import { Globe, ArrowRight, ArrowLeft, RotateCw, CornerDownRight, Plus, X, PenTool } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';

interface BrowserPanelProps {
  initialUrl?: string;
  currentUrl?: string;
  navigateUrl?: string;
}

interface TabInfo {
  id: string;
  url: string;
  title: string;
}

function normalizeBrowserUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim();
  if (!trimmed) return '';
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//i.test(trimmed)) return trimmed;
  if (/^about:/i.test(trimmed)) return trimmed;
  if (/^\//.test(trimmed)) return `file://${trimmed}`;
  return `https://${trimmed}`;
}

const DEFAULT_URL = 'about:blank';

export function BrowserPanel({ initialUrl, currentUrl, navigateUrl }: BrowserPanelProps) {
  const [url, setUrl] = useState(initialUrl || '');
  const [tabs, setTabs] = useState<TabInfo[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [annotationActive, setAnnotationActive] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const initializedRef = useRef(false);
  const browserOpenedRef = useRef(false);

  const activeTabIdRef = useRef<string | null>(null);

  const refreshTabs = useCallback(async () => {
    try {
      const list = await api.browserTabList();
      if (list.length === 0) {
        // 标签列表为空时创建一个空标签
        const tabId = await api.browserTabNew(DEFAULT_URL);
        activeTabIdRef.current = tabId;
        setActiveTabId(tabId);
        setUrl('');
        setTabs([{ id: tabId, url: DEFAULT_URL, title: '' }]);
      } else {
        setTabs(list);
        if (activeTabIdRef.current === null) {
          activeTabIdRef.current = list[0].id;
          setActiveTabId(list[0].id);
        }
      }
    } catch { /* ignore */ }
  }, []);

  useEffect(() => {
    if (currentUrl) {
      setUrl(currentUrl);
    }
  }, [currentUrl]);

  // 监听标签更新事件（使用 Tauri 的 listen API）
  useEffect(() => {
    let unlistenTab: (() => void) | null = null;
    let unlistenPage: (() => void) | null = null;

    const setup = async () => {
      unlistenTab = await listen('browser:tab_updated', () => { refreshTabs(); });
      unlistenPage = await listen('browser:page_loaded', (event) => {
        refreshTabs();
        // 同步 URL 栏
        const payload = event.payload as { url?: string };
        if (payload?.url) {
          setUrl(payload.url);
        }
      });
    };
    setup();

    return () => {
      unlistenTab?.();
      unlistenPage?.();
    };
  }, [refreshTabs]);

  useEffect(() => {
    if (!navigateUrl) return;
    if (browserOpenedRef.current) {
      api.browserNavigate(navigateUrl).catch(console.error);
    } else if (containerRef.current) {
      const rect = containerRef.current.getBoundingClientRect();
      if (rect.width > 0 && rect.height > 0) {
        api.browserOpen(navigateUrl, rect.x, rect.y, rect.width, rect.height)
          .then(() => { browserOpenedRef.current = true; })
          .then(() => refreshTabs())
          .catch(console.error);
      }
    }
  }, [navigateUrl, refreshTabs]);

  const syncPosition = useCallback(async () => {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height).catch(console.error);
  }, []);

  const handleNavigate = useCallback(async () => {
    const nextUrl = normalizeBrowserUrl(url);
    if (!nextUrl) return;

    try {
      if (!containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      if (browserOpenedRef.current) {
        await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height);
        await api.browserNavigate(nextUrl);
      } else {
        await api.browserOpen(nextUrl, rect.x, rect.y, rect.width, rect.height);
        browserOpenedRef.current = true;
      }
    } catch (err) {
      console.error('打开浏览器失败：', err);
    }
  }, [url]);

  const handleGoBack = useCallback(async () => {
    await api.browserGoBack().catch(console.error);
  }, []);

  const handleGoForward = useCallback(async () => {
    await api.browserGoForward().catch(console.error);
  }, []);

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

  const handleTabNew = useCallback(async () => {
    try {
      const tabId = await api.browserTabNew(DEFAULT_URL);
      setActiveTabId(tabId);
      setUrl('');
      await refreshTabs();
    } catch (err) {
      console.error('新建标签失败：', err);
    }
  }, [refreshTabs]);

  const handleTabSwitch = useCallback(async (tabId: string) => {
    try {
      await api.browserTabSwitch(tabId);
      activeTabIdRef.current = tabId;
      setActiveTabId(tabId);
      const tab = tabs.find(t => t.id === tabId);
      if (tab) setUrl(tab.url);
    } catch (err) {
      console.error('切换标签失败：', err);
    }
  }, [tabs]);

  const handleTabClose = useCallback(async (tabId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.browserTabClose(tabId);
      await refreshTabs();
    } catch (err) {
      console.error('关闭标签失败：', err);
    }
  }, [refreshTabs]);

  useEffect(() => {
    if (!containerRef.current) return;
    const observer = new ResizeObserver(() => {
      syncPosition();
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [syncPosition]);

  useEffect(() => {
    window.addEventListener('resize', syncPosition);
    window.addEventListener('tiangong:restore-browser-panel', syncPosition);
    return () => {
      window.removeEventListener('resize', syncPosition);
      window.removeEventListener('tiangong:restore-browser-panel', syncPosition);
    };
  }, [syncPosition]);

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
          .then(() => refreshTabs())
          .catch(console.error);
      };
      requestAnimationFrame(tryOpen);
      return () => { cancelled = true; };
    }
  }, [initialUrl, refreshTabs]);

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
        >
          <ArrowLeft className="w-3.5 h-3.5" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleGoForward}
          className="h-7 w-7 p-0 shrink-0"
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
      </div>

      {/* WebView 容器 */}
      <div
        ref={containerRef}
        className="flex-1 bg-muted/30"
      />
    </div>
  );
}
