import { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '@/api/tauri';
import { Globe, ArrowRight, ArrowLeft, RotateCw, CornerDownRight } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';

interface BrowserPanelProps {
  initialUrl?: string;
  currentUrl?: string;
}

function normalizeBrowserUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim();
  if (!trimmed) return '';
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  return `https://${trimmed}`;
}

export function BrowserPanel({ initialUrl, currentUrl }: BrowserPanelProps) {
  const [url, setUrl] = useState(initialUrl || 'https://www.bing.com');
  const containerRef = useRef<HTMLDivElement>(null);
  const initializedRef = useRef(false);
  const browserOpenedRef = useRef(false);

  // 同步后端推送的 URL 到地址栏
  useEffect(() => {
    if (currentUrl) {
      setUrl(currentUrl);
    }
  }, [currentUrl]);

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

  // 挂载时自动导航到 initialUrl（首次打开创建 webview，恢复时只重新定位）
  useEffect(() => {
    if (!initializedRef.current && containerRef.current) {
      initializedRef.current = true;
      const frame = requestAnimationFrame(() => {
        if (!containerRef.current) return;
        const rect = containerRef.current.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) return;
        api.browserOpen(initialUrl || 'https://www.bing.com', rect.x, rect.y, rect.width, rect.height)
          .then(() => {
            browserOpenedRef.current = true;
          })
          .catch(console.error);
      });
      return () => cancelAnimationFrame(frame);
    }
  }, [initialUrl]);

  return (
    <div className="flex flex-1 flex-col h-full border-l bg-background">
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
          <span className="text-xs">进入</span>
        </Button>
      </div>
      <div
        ref={containerRef}
        className="flex-1 bg-muted/30"
      />
    </div>
  );
}
