import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, RefreshCw, LogOut } from 'lucide-react';
import { Button } from './ui/button';
import { api } from '@/api/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
import appLogo from '../../../src-tauri/icons/128x128.png';

/**
 * 运行环境、插件加载、验证和常驻进程准备均完成后才挂载主界面。
 * 发送消息不承担启动准备；失败保留重试与退出入口。
 */
export function StartupPrepareGate({ children }: { children: React.ReactNode }) {
  const [checked, setChecked] = useState(false);
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const attempt = useRef(0);

  const runPrepare = useCallback(async () => {
    const current = ++attempt.current;
    setChecked(false);
    setReady(false);
    setFailed(null);
    try {
      await api.prepareStartupResources();
      // 状态机复检：以后端权威状态放行主界面。
      const state = await api.getSandboxUpdateState();
      if (current !== attempt.current) return;
      if (state.status === 'ready') {
        setReady(true);
      } else {
        setFailed(state.failure ?? `沙箱程序状态异常（${state.status}）`);
      }
    } catch (error) {
      if (current !== attempt.current) return;
      setFailed(String(error));
    } finally {
      if (current === attempt.current) setChecked(true);
    }
  }, []);

  useEffect(() => {
    void runPrepare();
    return () => {
      attempt.current += 1;
    };
  }, [runPrepare]);

  const handleRetry = () => {
    void runPrepare();
  };

  const handleExit = () => {
    void getCurrentWindow().destroy();
  };

  if (!checked) {
    return (
      <div className="flex min-h-screen w-full items-center justify-center bg-background p-6">
        <div className="w-full max-w-sm space-y-6 text-center" role="status" aria-live="polite" aria-busy="true">
          <div className="relative mx-auto flex h-20 w-20 items-center justify-center">
            <Loader2 className="absolute inset-0 h-20 w-20 animate-spin text-muted-foreground/40 motion-reduce:animate-none" strokeWidth={1} aria-hidden="true" />
            <img src={appLogo} alt="" className="h-12 w-12 object-contain motion-safe:animate-pulse" />
          </div>
          <div className="space-y-2">
            <h1 className="text-xl font-semibold">天工</h1>
            <p className="text-sm text-muted-foreground">
              天工正在启动中
            </p>
          </div>
        </div>
      </div>
    );
  }

  if (!ready) {
    return (
      <div className="flex h-screen w-full items-center justify-center bg-background p-6">
        <div className="w-full max-w-md space-y-6">
          <div className="space-y-1.5">
            <h1 className="flex items-center gap-2 text-lg font-semibold">
              {!failed && <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />}
              天工正在启动中
            </h1>
            <p className="text-sm text-muted-foreground">
              正在准备运行环境和插件，完成后自动进入应用。
            </p>
          </div>
          {failed && (
            <div className="space-y-3 rounded-md border border-destructive/40 bg-destructive/5 p-3">
              <p className="text-xs leading-relaxed text-destructive">
                运行环境准备失败：{failed}
                <br />
                请重试准备，或退出应用后重新启动。
              </p>
              <div className="flex gap-2">
                <Button size="sm" onClick={handleRetry}>
                  <RefreshCw className="mr-1 h-3 w-3" />
                  重试
                </Button>
                <Button size="sm" variant="outline" onClick={handleExit}>
                  <LogOut className="mr-1 h-3 w-3" />
                  退出应用
                </Button>
              </div>
            </div>
          )}
        </div>
      </div>
    );
  }

  return <>{children}</>;
}
