import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, AlertTriangle, X } from 'lucide-react';
import { api } from '@/api/tauri';
import type { StartupPrepareResult } from '@/api/tauri';
import { startWindowDrag } from '@/lib/windowDrag';
import appLogo from '../../../src-tauri/icons/128x128.png';

interface DegradedInfo {
  sandboxReason: string | null;
  pluginFailures: string[];
}

/**
 * 运行环境与插件就绪后挂载主界面；启动准备失败不再阻断进入——
 * 对话不依赖插件与沙箱，降级只影响工具，以横幅提示并在设置页可修复。
 */
export function StartupPrepareGate({ children }: { children: React.ReactNode }) {
  const [ready, setReady] = useState(false);
  const [degraded, setDegraded] = useState<DegradedInfo | null>(null);
  const [dismissed, setDismissed] = useState(false);
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
    let sandboxReason = result.degraded_reason;
    // 状态机复检：以后端权威状态为准；preparing 期间短暂轮询等待。
    for (let round = 0; round < 60; round += 1) {
      const state = await api.getSandboxUpdateState();
      if (current !== attempt.current) return;
      if (state.status === 'preparing') {
        await new Promise((resolve) => setTimeout(resolve, 500));
        continue;
      }
      if (state.status === 'failed' && state.failure) {
        sandboxReason = state.failure;
      }
      break;
    }
    if (current !== attempt.current) return;
    const pluginFailures = result.plugin_failures;
    if (sandboxReason || pluginFailures.length > 0) {
      setDegraded({ sandboxReason, pluginFailures });
    }
    setReady(true);
  }, []);

  useEffect(() => {
    void runPrepare();
    return () => {
      attempt.current += 1;
    };
  }, [runPrepare]);

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
        <div className="border-b border-amber-500/40 bg-amber-500/10" role="alert">
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
