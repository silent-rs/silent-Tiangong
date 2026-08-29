import { useState, useRef, useCallback } from "react";
import { api } from "@/api/tauri";

export type RecordingState = "idle" | "recording" | "transcribing";

/**
 * 录音 Hook：经 stt 插件 sidecar 录制麦克风音频。
 *
 * 录音由 stt 插件 sidecar 负责（record_start / record_stop / record_cancel），
 * 前端只负责状态编排与计时。停止录音后返回音频文件路径（~/.tiangong/media 下）。
 *
 * 防竞态：开始录音返回的会话 ID 会随停止/取消请求带回，sidecar 只对 ID 匹配
 * 的录音生效——快速"取消 → 再录"时迟到的旧取消请求不会误杀新录音；前端另在
 * 新录音开始前等待上一次取消落地（双保险）。
 */
export function useAudioRecording() {
  const [state, setState] = useState<RecordingState>("idle");
  const [duration, setDuration] = useState(0);

  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number>(0);
  // 当前录音会话 ID（record_start 返回），停止/取消请求携带。
  const sessionIdRef = useRef<string | null>(null);
  // 上一次取消的落地 Promise：新录音开始前等待它完成，保证旧取消请求
  // 先于新录音到达 sidecar。
  const pendingCancelRef = useRef<Promise<void> | null>(null);

  const cleanup = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const startRecording = useCallback(async () => {
    try {
      // 等待上一次取消落地后再开始新录音，避免旧取消请求迟到误杀新录音。
      if (pendingCancelRef.current) {
        await pendingCancelRef.current;
        pendingCancelRef.current = null;
      }
      // 会话 ID 由调用方生成：编号在请求发出前就已确定，停止/取消随后携带
      // 同一编号即可校验身份，不依赖开始录音响应的返回时序。
      const sessionId = crypto.randomUUID();
      await api.startRecording(sessionId);
      sessionIdRef.current = sessionId;
      setState("recording");
      setDuration(0);
      startTimeRef.current = Date.now();

      const startTime = Date.now();
      timerRef.current = setInterval(() => {
        setDuration(Math.floor((Date.now() - startTime) / 1000));
      }, 200);
    } catch (e: any) {
      console.error("启动录音失败:", e);
      // 透传 sidecar 的具体原因（如未安装 ffmpeg、已有录音占用），避免统一
      // 误导为权限问题。
      throw new Error(e?.message || "无法访问麦克风，请检查权限设置");
    }
  }, []);

  const stopRecording = useCallback(async (): Promise<{ filePath: string; mimeType: string }> => {
    const sessionId = sessionIdRef.current;
    try {
      const result = await api.stopRecording(sessionId ?? "");
      sessionIdRef.current = null;
      cleanup();
      setState("idle");
      return { filePath: result.file_path, mimeType: result.mime_type };
    } catch (e) {
      cleanup();
      setState("idle");
      throw e;
    }
  }, [cleanup]);

  const cancelRecording = useCallback(() => {
    cleanup();
    setState("idle");
    setDuration(0);
    // 通知 sidecar 终止本次会话（带 ID 校验）并丢弃文件；否则录音进程会一直
    // 占用麦克风，且下一次录音会报"已有录音在进行中"。失败只记录，不打断取消。
    const sessionId = sessionIdRef.current;
    sessionIdRef.current = null;
    const pending = api
      .cancelRecording(sessionId ?? "")
      .catch((e) => console.error("取消录音失败:", e))
      .finally(() => {
        if (pendingCancelRef.current === pending) pendingCancelRef.current = null;
      });
    pendingCancelRef.current = pending;
  }, [cleanup]);

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
