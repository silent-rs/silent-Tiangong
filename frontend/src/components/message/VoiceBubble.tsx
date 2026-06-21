import { useState } from "react";
import { Play, Square, ChevronUp, ChevronDown } from "lucide-react";
import { api } from "@/api/tauri";
import { useStore } from "@/store/useStore";

export function VoiceBubble({ messageId, audioPath, duration, showText, content }: {
  messageId: string; audioPath: string; duration?: number; showText: boolean; content: string;
}) {
  const [playing, setPlaying] = useState(false);
  const { toggleVoiceText } = useStore();
  const handlePlay = async () => {
    if (playing) { await api.stopAudio().catch(() => {}); setPlaying(false); return; }
    setPlaying(true);
    try { await api.playAudioFile(audioPath); } catch (e) { console.error("播放语音失败:", e); }
    setPlaying(false);
  };
  return (
    <div>
      <button className="flex items-center gap-2 text-sm hover:opacity-80 transition-opacity" onClick={handlePlay} title={playing ? "停止播放" : "点击播放语音"}>
        {playing ? <Square className="w-4 h-4 shrink-0" /> : <Play className="w-4 h-4 shrink-0 fill-current" />}
        <div className="flex items-center gap-1">
          <span className="inline-block w-16 h-[3px] rounded bg-foreground/40" />
          <span className="text-xs text-muted-foreground">{duration ? `${Math.round(duration)}″` : '语音'}</span>
        </div>
      </button>
      <div className="mt-1">
        <button className="text-xs text-muted-foreground hover:text-foreground transition-colors" onClick={() => toggleVoiceText(messageId)}>
          {showText ? <ChevronUp className="w-3 h-3 inline mr-0.5" /> : <ChevronDown className="w-3 h-3 inline mr-0.5" />}
          {showText ? '隐藏文字' : '显示文字'}
        </button>
      </div>
      {showText && <p className="whitespace-pre-wrap break-words text-sm mt-1 pt-1 border-t border-border">{content}</p>}
    </div>
  );
}
