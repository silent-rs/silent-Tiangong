import { useState, useRef, useEffect, useCallback } from "react";
import { api } from "@/api/tauri";

export type RecordingState = "idle" | "recording" | "transcribing";

/**
 * 录音 Hook：经 stt 插件 sidecar 录制麦克风音频。
 *
 * 录音由 stt 插件 sidecar 负责（record_start / record_stop / record_cancel），
 * 前端只负责状态编排与计时。停止录音后返回音频文件路径（~/.tiangong/media 下）。
 *
 * 防竞态与会话生命周期：
 * - 会话 ID 由本端生成，**请求发出前**即保存：开始录音的响应若在途中丢失，
 *   sidecar 可能已实际开始录音——此时用同一 ID 补发取消，不遗留失控进程；
 * - 停止/取消请求携带会话 ID，sidecar 只对匹配会话生效，快速"取消 → 再录"
 *   时迟到的旧取消不会误杀新录音；
 * - 新录音开始前等待上一次取消落地（双保险）；
 * - 组件卸载时清理计时器并取消进行中的录音，释放麦克风。
 */
export function useAudioRecording() {
  const [state, setState] = useState<RecordingState>("idle");
  const [duration, setDuration] = useState(0);

  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number>(0);
  // 当前录音会话 ID（本端生成），停止/取消请求携带。
  const sessionIdRef = useRef<string | null>(null);
  // 上一次取消的落地 Promise：新录音开始前等待它完成。
  const pendingCancelRef = useRef<Promise<void> | null>(null);
  // 卸载标记：卸载后不再更新 React 状态。
  const mountedRef = useRef(true);

  const safeSetState = useCallback((next: RecordingState) => {
    if (mountedRef.current) setState(next);
  }, []);

  const cleanup = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const trackCancel = useCallback((pending: Promise<void>) => {
    pendingCancelRef.current = pending;
  }, []);

  // 发送取消请求并登记落地 Promise（幂等：无会话/会话不匹配由 sidecar 静默处理）。
  const requestCancel = useCallback(
    (sessionId: string | null) => {
      const pending = api
        .cancelRecording(sessionId ?? "")
        .catch((e) => console.error("取消录音失败:", e))
        .finally(() => {
          if (pendingCancelRef.current === pending) pendingCancelRef.current = null;
        });
      trackCancel(pending);
    },
    [trackCancel],
  );

  // 卸载清理：清计时器 + 取消进行中的录音，麦克风不因组件卸载而被占用。
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      cleanup();
      const sessionId = sessionIdRef.current;
      if (sessionId) {
        sessionIdRef.current = null;
        requestCancel(sessionId);
      }
    };
  }, [cleanup, requestCancel]);

  const startRecording = useCallback(async () => {
    try {
      // 等待上一次取消落地后再开始新录音，避免旧取消请求迟到误杀新录音。
      if (pendingCancelRef.current) {
        await pendingCancelRef.current;
        pendingCancelRef.current = null;
      }
      // 会话 ID 在请求发出前保存：即使响应丢失，也能用同一 ID 补发取消。
      const sessionId = crypto.randomUUID();
      sessionIdRef.current = sessionId;
      try {
        await api.startRecording(sessionId);
      } catch (startError) {
        // sidecar 可能已实际开始录音（响应在返回途中失败）：
        // 用同一会话 ID 补发取消并等待落地，再清除本地会话。
        sessionIdRef.current = null;
        const pending = api
          .cancelRecording(sessionId)
          .catch((e) => console.error("启动失败后补发取消失败:", e))
          .then(() => {
            if (pendingCancelRef.current === pending) pendingCancelRef.current = null;
          });
        trackCancel(pending);
        throw startError;
      }
      safeSetState("recording");
      setDuration(0);
      startTimeRef.current = Date.now();

      const startTime = Date.now();
      timerRef.current = setInterval(() => {
        setDuration(Math.floor((Date.now() - startTime) / 1000));
      }, 200);
    } catch (e: any) {
      console.error("启动录音失败:", e);
      // 透传 sidecar 的具体原因（如无麦克风设备、已有录音占用），避免统一
      // 误导为权限问题。
      throw new Error(e?.message || "无法访问麦克风，请检查权限设置");
    }
  }, [safeSetState, trackCancel]);

  const stopRecording = useCallback(async (): Promise<{ filePath: string; mimeType: string }> => {
    const sessionId = sessionIdRef.current;
    try {
      const result = await api.stopRecording(sessionId ?? "");
      sessionIdRef.current = null;
      cleanup();
      safeSetState("idle");
      return { filePath: result.file_path, mimeType: result.mime_type };
    } catch (e) {
      cleanup();
      safeSetState("idle");
      throw e;
    }
  }, [cleanup, safeSetState]);

  const cancelRecording = useCallback(() => {
    cleanup();
    safeSetState("idle");
    setDuration(0);
    // 通知 sidecar 终止本次会话（带 ID 校验）并丢弃文件；否则录音进程会一直
    // 占用麦克风，且下一次录音会报"已有录音在进行中"。失败只记录，不打断取消。
    const sessionId = sessionIdRef.current;
    sessionIdRef.current = null;
    requestCancel(sessionId);
  }, [cleanup, requestCancel, safeSetState]);

  /** 获取从开始录音到现在的精确毫秒数 */
  const getElapsedMs = useCallback(() => {
    return startTimeRef.current > 0 ? Date.now() - startTimeRef.current : 0;
  }, []);

  return {
    state,
    setState,
    duration,
    startRecording,
    stopRecording,
    cancelRecording,
    getElapsedMs,
  };
}
