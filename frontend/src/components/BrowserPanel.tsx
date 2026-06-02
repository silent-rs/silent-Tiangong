import { useState, useRef, useEffect, useCallback } from 'react';
import { api } from '@/api/tauri';
import { Globe, ArrowRight, ArrowLeft, RotateCw } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';

const TOOLBAR_HEIGHT = 44;

interface BrowserPanelProps {
  initialUrl?: string;
  currentUrl?: string;
}

export function BrowserPanel({ initialUrl, currentUrl }: BrowserPanelProps) {
  const [url, setUrl] = useState(initialUrl || 'https://www.bing.com');
  const containerRef = useRef<HTMLDivElement>(null);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const initializedRef = useRef(false);
  const [browserSize, setBrowserSize] = useState({ width: 0, height: 0 });

  // 同步后端推送的 URL 到地址栏
  useEffect(() => {
    if (currentUrl) {
      setUrl(currentUrl);
    }
  }, [currentUrl]);

  // 根据 wrapper 可用高度计算 4:3 比例的浏览器尺寸
  const recalcSize = useCallback(() => {
    if (!wrapperRef.current) return;
    const wrapperHeight = wrapperRef.current.clientHeight;
    const availableHeight = wrapperHeight - TOOLBAR_HEIGHT;
    if (availableHeight <= 0) return;
    // 4:3 比例：宽度 = 高度 * 4/3
    const width = Math.floor(availableHeight * 4 / 3);
    const height = availableHeight;
    setBrowserSize({ width, height });
  }, []);

  const syncPosition = useCallback(async () => {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    await api.browserSetPosition(rect.x, rect.y, rect.width, rect.height).catch(console.error);
  }, []);

  const handleNavigate = useCallback(async () => {
    if (!url.trim()) return;

    try {
      if (!containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      await api.browserOpen(url, rect.x, rect.y, rect.width, rect.height);
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

  // 监听 wrapper 尺寸变化，重算 4:3 比例
  useEffect(() => {
    if (!wrapperRef.current) return;
    const observer = new ResizeObserver(() => {
      recalcSize();
    });
    observer.observe(wrapperRef.current);
    return () => observer.disconnect();
  }, [recalcSize]);

  // 尺寸变化时同步 WebView 位置
  useEffect(() => {
    if (browserSize.width > 0 && browserSize.height > 0) {
      syncPosition();
    }
  }, [browserSize, syncPosition]);

  useEffect(() => {
    window.addEventListener('resize', recalcSize);
    return () => window.removeEventListener('resize', recalcSize);
  }, [recalcSize]);

  // 挂载时自动导航到 initialUrl
  useEffect(() => {
    if (!initializedRef.current && containerRef.current && browserSize.width > 0) {
      initializedRef.current = true;
      const rect = containerRef.current.getBoundingClientRect();
      api.browserOpen(initialUrl || 'https://www.bing.com', rect.x, rect.y, rect.width, rect.height).catch(console.error);
    }
  }, [initialUrl, browserSize]);

  return (
    <div ref={wrapperRef} className="flex flex-col h-full border-l bg-background">
      <div className="flex items-center gap-1 px-2 py-2 border-b" style={{ height: TOOLBAR_HEIGHT }}>
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
          onKeyDown={(e) => e.key === 'Enter' && handleNavigate()}
          placeholder="输入 URL..."
          className="flex-1 h-7 text-sm"
        />
      </div>
      <div className="flex-1 flex justify-center overflow-hidden">
        {browserSize.width > 0 && (
          <div
            ref={containerRef}
            className="bg-muted/30"
            style={{ width: browserSize.width, height: browserSize.height }}
          />
        )}
      </div>
    </div>
  );
}
