import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, AlertTriangle, X, RefreshCw } from 'lucide-react';
import { api } from '@/api/tauri';
import type { SandboxUpdateState, StartupPrepareResult } from '@/api/tauri';
import { startWindowDrag } from '@/lib/windowDrag';
import appLogo from '../../../src-tauri/icons/128x128.png';

interface DegradedInfo {
  sandboxReason: string | null;
  pluginFailures: string[];
  /** 沙箱仍在准备中：后台续查，就绪后横幅自动消失。 */
  pending: boolean;
}

/** 组装降级信息；state 查询失败时保留启动结果中的原因。 */
async function collectDegraded(result: StartupPrepareResult): Promise<DegradedInfo> {
  let sandboxReason = result.degraded_reason;
  const state: SandboxUpdateState | null = await api.getSandboxUpdateState().catch(() => null);
  if (state?.status === 'failed' && state.failure) {
    sandboxReason = state.failure;
  }
  return {
    sandboxReason,
    pluginFailures: result.plugin_failures,
    pending: state?.status === 'preparing' && sandboxReason === null,
  };
}

/**
 * 运行环境与插件就绪后挂载主界面；启动准备失败不再阻断进入——
 * 对话不依赖插件与沙箱，降级只影响工具，以浮层横幅提示并在设置页可修复。
 */
export function StartupPrepareGate({ children }: { children: React.ReactNode }) {
  const [ready, setReady] = useState(false);
  const [degraded, setDegraded] = useState<DegradedInfo | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const attempt = useRef(0);

  const runPrepare = useCallback(async () => {
    const current = ++attempt.current;
    setReady(false);
    setDegraded(null);
    const result: StartupPrepareResult = await api.prepareStartupResources().catch(
      (error: unknown) =>
        // 后端已把沙箱/插件失败转为降级结果；这里的异常属于意外，同样放行。
        ({
          installed_version: null,
          degraded_reason: String(error),
          plugin_failures: [],
        }) satisfies StartupPrepareResult,
    );
    if (current !== attempt.current) return;
    // 状态机复检：以后端权威状态为准；沙箱准备超过短窗口即降级放行，
    // 后台续查，就绪后横幅自动消失，不再让用户停在启动页等待。
    for (let round = 0; round < 10; round += 1) {
      const state = await api.getSandboxUpdateState();
      if (current !== attempt.current) return;
      if (state.status === 'preparing') {
        await new Promise((resolve) => setTimeout(resolve, 500));
        continue;
      }
      break;
    }
    if (current !== attempt.current) return;
    const info = await collectDegraded(result);
    if (current !== attempt.current) return;
    if (info.pending) {
      info.sandboxReason = '沙箱程序仍在准备中，插件工具暂不可用';
    }
    setDegraded(
      info.sandboxReason || info.pluginFailures.length > 0 ? info : null,
    );
    setReady(true);
  }, []);

  useEffect(() => {
    void runPrepare();
    return () => {
      attempt.current += 1;
    };
  }, [runPrepare]);

  // 沙箱仍在准备时后台续查：就绪清除横幅，失败更新原因。
  useEffect(() => {
    if (!degraded?.pending) return;
    const timer = setInterval(() => {
      void api
        .getSandboxUpdateState()
        .then((state) => {
          if (state.status === 'preparing') return;
          setDegraded((current) => {
            if (!current?.pending) return current;
            if (state.status === 'ready') {
              return current.pluginFailures.length > 0
                ? { sandboxReason: null, pluginFailures: current.pluginFailures, pending: false }
                : null;
            }
            return {
              sandboxReason: state.failure ?? current.sandboxReason,
              pluginFailures: current.pluginFailures,
              pending: false,
            };
          });
        })
        .catch(() => undefined);
    }, 2000);
    return () => clearInterval(timer);
  }, [degraded?.pending]);

  // 横幅上的重试：后端命令自带失败插件重试，完成后刷新降级信息，
  // 不回启动页打断已进入的会话。
  const handleRetry = useCallback(async () => {
    setRetrying(true);
    try {
      const result: StartupPrepareResult = await api.prepareStartupResources().catch(
        (error: unknown) => ({
          installed_version: null,
          degraded_reason: String(error),
          plugin_failures: [],
        }) satisfies StartupPrepareResult,
      );
      const info = await collectDegraded(result);
      if (info.pending) {
        info.sandboxReason = '沙箱程序仍在准备中，插件工具暂不可用';
      }
      setDegraded(
        info.sandboxReason || info.pluginFailures.length > 0 ? info : null,
      );
    } finally {
      setRetrying(false);
    }
  }, []);

  // 窗口为 macOS Overlay 标题栏，启动期间无系统拖动区；header 与主界面一致提供拖动。
  if (!ready) {
    return (
      <div className="flex h-screen w-full flex-col bg-background">
        <header
          className="flex h-12 shrink-0 border-b select-none"
          onMouseDown={startWindowDrag}
        />
        <div className="flex min-h-0 w-full flex-1 items-center justify-center p-6">
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
      </div>
    );
  }

  return (
    <>
      {degraded && !dismissed && (
        <div
          className="fixed inset-x-0 top-0 z-[100] border-b border-amber-500/40 bg-amber-500/10 backdrop-blur-sm"
          role="alert"
        >
          <div className="mx-auto flex max-w-3xl items-start gap-2 px-4 py-2 text-xs leading-5 text-amber-900 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden="true" />
            <div className="min-w-0 flex-1 space-y-1">
              {degraded.sandboxReason && (
                <p className="break-words">
                  沙箱程序不可用，插件工具暂不可用（对话不受影响）：{degraded.sandboxReason}
                  {' '}可在 设置 → 沙箱管理 中检查并更新后自动恢复。
                </p>
              )}
              {degraded.pluginFailures.length > 0 && (
                <p className="break-words">
                  {degraded.pluginFailures.length} 个插件启动失败，相关工具暂不可用（对话不受影响），详情见 设置 → 插件管理。
                </p>
              )}
            </div>
            <button
              type="button"
              className="flex shrink-0 items-center gap-1 rounded border border-amber-500/40 px-1.5 py-0.5 hover:bg-amber-500/20 disabled:opacity-50"
              aria-label="重试启动准备"
              disabled={retrying}
              onClick={() => void handleRetry()}
            >
              {retrying ? (
                <Loader2 className="h-3 w-3 animate-spin" aria-hidden="true" />
              ) : (
                <RefreshCw className="h-3 w-3" aria-hidden="true" />
              )}
              重试
            </button>
            <button
              type="button"
              className="rounded p-0.5 hover:bg-amber-500/20"
              aria-label="关闭提示"
              onClick={() => setDismissed(true)}
            >
              <X className="h-3.5 w-3.5" aria-hidden="true" />
            </button>
          </div>
        </div>
      )}
      {children}
    </>
  );
}
