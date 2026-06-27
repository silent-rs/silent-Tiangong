import { useState } from "react";
import { Copy, Check, Volume2, Square, Loader2, Clock } from "lucide-react";
import { api } from "@/api/tauri";

/** 将毫秒格式化为人类可读时长：< 1s 显示 ms，否则显示 s（保留 1 位小数）。 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function MessageActions({ text, showTts, durationMs }: { text: string; showTts: boolean; durationMs?: number | null }) {
  const [copied, setCopied] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [ttsLoading, setTtsLoading] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) { console.error("复制失败:", e); }
  };

  const handleTts = async () => {
    if (playing) { api.stopAudio().catch(() => {}); setPlaying(false); return; }
    setTtsLoading(true);
    try {
      api.stopAudio().catch(() => {});
      const result = await api.synthesizeSpeech(text);
      setPlaying(true);
      setTtsLoading(false);
      await api.playAudioFile(result.file_path);
      setPlaying(false);
    } catch (e: any) {
      console.error("TTS 播放失败:", e);
      alert(`语音播放失败：${e?.message || e}`);
      setPlaying(false);
      setTtsLoading(false);
    }
  };

  const btnClass = "p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors";
  return (
    <div className="flex items-center gap-0.5 mt-1">
      <button onClick={handleCopy} className={btnClass} title={copied ? "已复制" : "复制"}>
        {copied ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />}
      </button>
      {showTts && (
        <button onClick={handleTts} className={btnClass} title={playing ? "停止播放" : "朗读"}>
          {ttsLoading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : playing ? <Square className="w-3.5 h-3.5" /> : <Volume2 className="w-3.5 h-3.5" />}
        </button>
      )}
      {durationMs != null && durationMs > 0 && (
        <span className="inline-flex items-center gap-0.5 ml-1 pl-1 border-l border-border/60 text-[11px] text-muted-foreground/70 tabular-nums" title="本轮执行总时长">
          <Clock className="w-3 h-3" />
          {formatDuration(durationMs)}
        </span>
      )}
    </div>
  );
}
