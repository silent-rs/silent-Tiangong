import { useState, useRef, useCallback } from "react";

export type RecordingState = "idle" | "recording" | "transcribing";

const TARGET_SAMPLE_RATE = 16000;

/** 降采样：从原始采样率降到目标采样率 */
function downsample(samples: Float32Array, fromRate: number, toRate: number): Float32Array {
  if (fromRate === toRate) return samples;
  const ratio = fromRate / toRate;
  const newLength = Math.round(samples.length / ratio);
  const result = new Float32Array(newLength);
  for (let i = 0; i < newLength; i++) {
    const srcIndex = i * ratio;
    const low = Math.floor(srcIndex);
    const high = Math.min(low + 1, samples.length - 1);
    const frac = srcIndex - low;
    result[i] = samples[low] * (1 - frac) + samples[high] * frac;
  }
  return result;
}

/** 将 Float32Array PCM 数据编码为 WAV Blob */
function encodeWav(samples: Float32Array, sampleRate: number): Blob {
  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);

  const writeString = (offset: number, str: string) => {
    for (let i = 0; i < str.length; i++) {
      view.setUint8(offset + i, str.charCodeAt(i));
    }
  };

  writeString(0, "RIFF");
  view.setUint32(4, 36 + samples.length * 2, true);
  writeString(8, "WAVE");
  writeString(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeString(36, "data");
  view.setUint32(40, samples.length * 2, true);

  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }

  return new Blob([buffer], { type: "audio/wav" });
}

/**
 * 录音 Hook：使用 AudioContext 录制麦克风音频，编码为 WAV
 * 使用设备默认采样率录制，停止时降采样到 16kHz
 */
export function useAudioRecording() {
  const [state, setState] = useState<RecordingState>("idle");
  const [duration, setDuration] = useState(0);

  const audioCtxRef = useRef<AudioContext | null>(null);
  const sourceRef = useRef<MediaStreamAudioSourceNode | null>(null);
  const processorRef = useRef<ScriptProcessorNode | null>(null);
  const chunksRef = useRef<Float32Array[]>([]);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const startTimeRef = useRef<number>(0);

  const cleanup = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
    if (processorRef.current) {
      processorRef.current.onaudioprocess = null;
      processorRef.current.disconnect();
      processorRef.current = null;
    }
    if (sourceRef.current) {
      sourceRef.current.disconnect();
      sourceRef.current = null;
    }
    if (audioCtxRef.current) {
      audioCtxRef.current.close().catch(() => {});
      audioCtxRef.current = null;
    }
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
    chunksRef.current = [];
  }, []);

  const startRecording = useCallback(async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;

      // 使用设备默认采样率，避免初始化延迟
      const audioCtx = new AudioContext();
      audioCtxRef.current = audioCtx;

      const source = audioCtx.createMediaStreamSource(stream);
      sourceRef.current = source;

      const processor = audioCtx.createScriptProcessor(4096, 1, 1);
      processorRef.current = processor;
      chunksRef.current = [];

      processor.onaudioprocess = (e) => {
        const data = e.inputBuffer.getChannelData(0);
        chunksRef.current.push(new Float32Array(data));
      };

      source.connect(processor);
      processor.connect(audioCtx.destination);

      setState("recording");
      setDuration(0);
      startTimeRef.current = Date.now();

      const startTime = Date.now();
      timerRef.current = setInterval(() => {
        setDuration(Math.floor((Date.now() - startTime) / 1000));
      }, 200);
    } catch (e) {
      console.error("获取麦克风失败:", e);
      throw new Error("无法访问麦克风，请检查权限设置");
    }
  }, []);

  const stopRecording = useCallback((): Promise<{ blob: Blob; mimeType: string }> => {
    return new Promise((resolve, reject) => {
      const audioCtx = audioCtxRef.current;
      if (!audioCtx || chunksRef.current.length === 0) {
        cleanup();
        reject(new Error("未在录音中"));
        return;
      }

      const nativeSampleRate = audioCtx.sampleRate;
      const totalLength = chunksRef.current.reduce((acc, c) => acc + c.length, 0);
      const merged = new Float32Array(totalLength);
      let offset = 0;
      for (const chunk of chunksRef.current) {
        merged.set(chunk, offset);
        offset += chunk.length;
      }

      // 降采样到 16kHz
      const resampled = downsample(merged, nativeSampleRate, TARGET_SAMPLE_RATE);
      const blob = encodeWav(resampled, TARGET_SAMPLE_RATE);

      cleanup();
      setState("idle");
      resolve({ blob, mimeType: "audio/wav" });
    });
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
