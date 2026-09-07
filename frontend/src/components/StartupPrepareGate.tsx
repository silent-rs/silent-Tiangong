import { useCallback, useEffect, useState } from 'react';
import { Loader2, RefreshCw, LogOut, ArrowRight } from 'lucide-react';
import { Button } from './ui/button';
import { api } from '@/api/tauri';
import { getCurrentWindow } from '@tauri-apps/api/window';
import appLogo from '../../../src-tauri/icons/128x128.png';

/**
 * 启动准备门（首装引导，非强制门）：Launcher 缺失时默认先自动完成
 * 下载、校验与直接激活（无需重启），完成即进入主界面。
 *
 * 沙箱影响终端、命令、解释器及其他按需 Sidecar——对话与纯 UI 浏览不依赖它，且 Sidecar 侧
 * 对未就绪状态始终 fail-closed。因此准备中或准备失败都不阻塞应用：
 * 用户可随时选择"先进入应用"继续对话，沙箱可在设置页重试准备
 * （沙箱程序更新 → 检查并更新），就绪前命令类功能保持明确拒绝。
 */
export function StartupPrepareGate({ children }: { children: React.ReactNode }) {
  const [checked, setChecked] = useState(false);
  const [ready, setReady] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const [enterRequested, setEnterRequested] = useState(false);

  const runPrepare = useCallback(async () => {
    setFailed(null);
    try {
      await api.prepareStartupResources();
      // 状态机复检：以后端权威状态放行主界面。
      const state = await api.getSandboxUpdateState();
      if (state.status === 'ready') {
        setReady(true);
      } else {
        setFailed(state.failure ?? `沙箱程序状态异常（${state.status}）`);
      }
    } catch (error) {
      setFailed(String(error));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const state = await api.getSandboxUpdateState();
        if (cancelled) return;
        if (state.status === 'ready') {
          setReady(true);
          setChecked(true);
          return;
        }
        // missing / failed / preparing：进入首装准备流程（可跳过进入应用）。
        setChecked(true);
        if (state.status === 'failed' && state.failure) {
          setFailed(state.failure);
          return;
        }
        void runPrepare();
      } catch (error) {
        if (cancelled) return;
        setChecked(true);
        setFailed(String(error));
      }
    })();
    return () => {
      cancelled = true;
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

  if (!ready && !enterRequested) {
    return (
      <div className="flex h-screen w-full items-center justify-center bg-background p-6">
        <div className="w-full max-w-md space-y-6">
          <div className="space-y-1.5">
            <h1 className="flex items-center gap-2 text-lg font-semibold">
              {!failed && <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />}
              天工正在启动中
            </h1>
            <p className="text-sm text-muted-foreground">
              首次使用会自动下载并验证沙箱程序，完成后按需 Sidecar 功能即可用。
              对话与浏览不依赖沙箱，可随时先进入应用继续使用。
            </p>
          </div>
          {failed && (
            <div className="space-y-3 rounded-md border border-destructive/40 bg-destructive/5 p-3">
              <p className="text-xs leading-relaxed text-destructive">
                运行环境准备失败：{failed}
                <br />
                不影响对话与浏览；可稍后在设置 → 沙箱程序更新中重试。
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
          <Button
            variant="ghost"
            className="h-8 text-xs text-muted-foreground"
            onClick={() => setEnterRequested(true)}
          >
            <ArrowRight className="mr-1 h-3 w-3" />
            {failed ? '先进入应用（命令类功能暂不可用）' : '跳过等待，先进入应用'}
          </Button>
        </div>
      </div>
    );
  }

  return <>{children}</>;
}
