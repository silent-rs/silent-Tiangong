import { useEffect, useRef, useCallback, useState } from "react";
import { api } from "@/api/tauri";
import { useStore } from "@/store/useStore";

// 中英文句子边界正则：句号、感叹号、问号、分号、换行
const SENTENCE_BOUNDARY = /([。！？；\n]|(?<=[.!?])\s)/;

/**
 * 流式 TTS Hook：监听 LLM 流式输出，按句切分，逐句合成并顺序播放
 */
export function useStreamingTts() {
  const [enabled, setEnabled] = useState(false);
  const [isPlaying, setIsPlaying] = useState(false);

  // 已发送到 TTS 的文本长度（字符偏移）
  const sentOffsetRef = useRef(0);
  // 待合成的句子队列
  const sentenceQueueRef = useRef<string[]>([]);
  // 音频文件播放队列
  const audioQueueRef = useRef<string[]>([]);
  // 当前是否正在合成
  const synthesizingRef = useRef(false);
  // 当前是否正在播放
  const playingRef = useRef(false);
  // 上一次的 streamingMessageId，用于检测新会话
  const lastStreamingIdRef = useRef<string | null>(null);
  // 流式是否已结束（用于 flush 残余文本）
  const streamEndedRef = useRef(false);

  const { streamingMessageId, streamingContent } = useStore();

  // 重置所有状态
  const reset = useCallback(() => {
    sentOffsetRef.current = 0;
    sentenceQueueRef.current = [];
    audioQueueRef.current = [];
    synthesizingRef.current = false;
    playingRef.current = false;
    streamEndedRef.current = false;
    api.stopAudio().catch(() => {});
    setIsPlaying(false);
  }, []);

  // 停止播放
  const stop = useCallback(() => {
    reset();
    setEnabled(false);
  }, [reset]);

  // 顺序播放音频队列（通过系统原生命令）
  const playNext = useCallback(async () => {
    if (playingRef.current) return;
    if (audioQueueRef.current.length === 0) {
      setIsPlaying(false);
      return;
    }

    playingRef.current = true;
    setIsPlaying(true);

    while (audioQueueRef.current.length > 0) {
      const filePath = audioQueueRef.current.shift()!;
      try {
        // afplay 会阻塞直到播放完成，然后自动播放下一个
        await api.playAudioFile(filePath);
      } catch (e) {
        console.error("流式 TTS 播放失败:", e);
      }
    }

    playingRef.current = false;
    setIsPlaying(false);
  }, []);

  // 从句子队列取出句子进行合成
  const processSynthesisQueue = useCallback(async () => {
    if (synthesizingRef.current) return;
    if (sentenceQueueRef.current.length === 0) return;

    synthesizingRef.current = true;

    while (sentenceQueueRef.current.length > 0) {
      const sentence = sentenceQueueRef.current.shift()!;
      if (!sentence.trim()) continue;

      try {
        const result = await api.synthesizeSpeech(sentence);
        audioQueueRef.current.push(result.file_path);
        // 触发播放（如果尚未在播放）
        playNext();
      } catch (e) {
        console.error("流式 TTS 合成失败:", e);
      }
    }

    synthesizingRef.current = false;
  }, [playNext]);

  // 从新增文本中提取完整句子
  const extractSentences = useCallback((text: string, flush: boolean) => {
    const unprocessed = text.slice(sentOffsetRef.current);
    if (!unprocessed) return;

    const parts = unprocessed.split(SENTENCE_BOUNDARY);
    let accumulated = "";
    const sentences: string[] = [];

    for (const part of parts) {
      accumulated += part;
      // 如果当前部分是句子边界，则将已累积的文本作为一个句子
      if (SENTENCE_BOUNDARY.test(part) && accumulated.trim()) {
        sentences.push(accumulated.trim());
        sentOffsetRef.current += accumulated.length;
        accumulated = "";
      }
    }

    // flush 模式：流式结束时将残余文本也作为最后一句
    if (flush && accumulated.trim()) {
      sentences.push(accumulated.trim());
      sentOffsetRef.current += accumulated.length;
    }

    if (sentences.length > 0) {
      sentenceQueueRef.current.push(...sentences);
      processSynthesisQueue();
    }
  }, [processSynthesisQueue]);

  // 监听流式内容变化
  useEffect(() => {
    if (!enabled) return;

    // 检测新的流式会话开始
    if (streamingMessageId && streamingMessageId !== lastStreamingIdRef.current) {
      reset();
      lastStreamingIdRef.current = streamingMessageId;
      streamEndedRef.current = false;
    }

    // 流式进行中：提取新句子
    if (streamingMessageId && streamingContent) {
      extractSentences(streamingContent, false);
    }

    // 流式结束：flush 残余文本
    if (!streamingMessageId && lastStreamingIdRef.current && !streamEndedRef.current) {
      streamEndedRef.current = true;
      // 从 store 获取最终完整文本
      const finalMessages = useStore.getState().messages;
      const lastMsg = finalMessages[finalMessages.length - 1];
      if (lastMsg && lastMsg.role === "assistant") {
        extractSentences(lastMsg.content, true);
      }
      lastStreamingIdRef.current = null;
    }
  }, [enabled, streamingMessageId, streamingContent, extractSentences, reset]);

  return { enabled, setEnabled, isPlaying, stop };
}
