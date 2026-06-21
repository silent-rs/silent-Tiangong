import { useState } from "react";
import { Copy, Check, Volume2, Square, Loader2 } from "lucide-react";
import { api } from "@/api/tauri";

export function MessageActions({ text, showTts }: { text: string; showTts: boolean }) {
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
    </div>
  );
}
