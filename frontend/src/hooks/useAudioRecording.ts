import { useState, useRef, useCallback } from "react";
import { api } from "@/api/tauri";

export type RecordingState = "idle" | "recording" | "transcribing";

/**
 * 录音 Hook：经 stt 插件 sidecar 录制麦克风音频。
 *
 * 录音由 stt 插件 sidecar 负责（record_start / record_stop），前端只负责
 * 状态编排与计时。停止录音后返回音频文件路径（~/.tiangong/media 下）。
 */
export function useAudioRecording() {
  const [state, setState] = useState<RecordingState>("idle");
  const [duration, setDuration] = useState(0);

  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startTimeRef = useRef<number>(0);

  const cleanup = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const startRecording = useCallback(async () => {
    try {
      await api.startRecording();
      setState("recording");
      setDuration(0);
      startTimeRef.current = Date.now();

      const startTime = Date.now();
      timerRef.current = setInterval(() => {
        setDuration(Math.floor((Date.now() - startTime) / 1000));
      }, 200);
    } catch (e) {
      console.error("启动录音失败:", e);
      throw new Error("无法访问麦克风，请检查权限设置");
    }
  }, []);

  const stopRecording = useCallback(async (): Promise<{ filePath: string; mimeType: string }> => {
    try {
      const result = await api.stopRecording();
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