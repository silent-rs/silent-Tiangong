import { useState, KeyboardEvent, useEffect, useRef, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square, FolderOpen, Wrench, Cpu, Mic, Loader2, Keyboard, MessageSquarePlus } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { api } from '@/api/tauri';
import { useAudioRecording } from '@/hooks/useAudioRecording';

interface MentionCandidate {
  value: string;
  label: string;
  kind: string;
  hint: string;
}

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus, isDraft, activeSessionId, sessionRunStatuses, sessionCwd, setSessionCwd, addVoiceMessage } = useStore();
  const [isComposing, setIsComposing] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // @提及补全状态
  const [mentionOpen, setMentionOpen] = useState(false);
  const [mentionCandidates, setMentionCandidates] = useState<MentionCandidate[]>([]);
  const [mentionFilter, setMentionFilter] = useState('');
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionStart, setMentionStart] = useState(-1);
  const mentionRef = useRef<HTMLDivElement>(null);

  // STT 录音
  const [hasStt, setHasStt] = useState(false);
  const [voiceMode, setVoiceMode] = useState(false);
  const [voiceCancelled, setVoiceCancelled] = useState(false);
  const [voiceTooShort, setVoiceTooShort] = useState(false);
  const recording = useAudioRecording();
  const isRecordingRef = useRef(false);

  useEffect(() => {
    api.hasSttCapability().then(setHasStt).catch(() => setHasStt(false));
  }, []);

  // 当前会话是否空闲
  const currentSessionStatus = isDraft
    ? 'idle'
    : (activeSessionId && sessionRunStatuses[activeSessionId]) || runStatus;
  const isIdle = currentSessionStatus === 'idle';
  const canSend = inputContent.trim().length > 0;  // 执行中也允许输入

  // 自动调整文本框高度
  useEffect(() => {
    const textarea = textareaRef.current;
    if (textarea) {
      textarea.style.height = '60px';
      textarea.style.height = Math.min(textarea.scrollHeight, 200) + 'px';
    }
  }, [inputContent]);

  // ===== 文字模式相关 =====
  const loadCandidates = useCallback(async () => {
    try {
      const candidates = await api.getMentionCandidates();
      setMentionCandidates(candidates);
    } catch (e) {
      console.error('加载提及候选失败:', e);
    }
  }, []);

  const filteredCandidates = mentionCandidates.filter(c => {
    if (!mentionFilter) return true;
    const lower = mentionFilter.toLowerCase();
    return c.label.toLowerCase().includes(lower)
      || c.value.toLowerCase().includes(lower)
      || c.hint.toLowerCase().includes(lower);
  });

  const handleInputChange = (value: string) => {
    setInputContent(value);
    const textarea = textareaRef.current;
    if (!textarea) return;
    const cursorPos = textarea.selectionStart;
    let atPos = -1;
    for (let i = cursorPos - 1; i >= 0; i--) {
      const ch = value[i];
      if (ch === '@') {
        if (i === 0 || /\s/.test(value[i - 1])) { atPos = i; }
        break;
      }
      if (/\s/.test(ch)) break;
    }
    if (atPos >= 0) {
      const filter = value.slice(atPos + 1, cursorPos);
      setMentionStart(atPos);
      setMentionFilter(filter);
      setMentionIndex(0);
      if (!mentionOpen) { loadCandidates(); setMentionOpen(true); }
    } else {
      setMentionOpen(false);
    }
  };

  const selectCandidate = (candidate: MentionCandidate) => {
    if (mentionStart < 0) return;
    const textarea = textareaRef.current;
    const cursorPos = textarea?.selectionStart ?? inputContent.length;
    const before = inputContent.slice(0, mentionStart);
    const after = inputContent.slice(cursorPos);
    const newValue = `${before}${candidate.value} ${after}`;
    setInputContent(newValue);
    setMentionOpen(false);
    setTimeout(() => {
      if (textarea) {
        const newPos = mentionStart + candidate.value.length + 1;
        textarea.focus();
        textarea.setSelectionRange(newPos, newPos);
      }
    }, 0);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (mentionOpen && filteredCandidates.length > 0) {
      if (e.key === 'ArrowDown') { e.preventDefault(); setMentionIndex(i => (i + 1) % filteredCandidates.length); return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); setMentionIndex(i => (i - 1 + filteredCandidates.length) % filteredCandidates.length); return; }
      if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey) { e.preventDefault(); selectCandidate(filteredCandidates[mentionIndex]); return; }
      if (e.key === 'Escape') { e.preventDefault(); setMentionOpen(false); return; }
      if (e.key === 'Tab') { e.preventDefault(); selectCandidate(filteredCandidates[mentionIndex]); return; }
    }
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && !isComposing) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleSend = async () => {
    if (!canSend) return;
    setMentionOpen(false);
    if (isIdle) {
      sendMessage(inputContent);
    } else {
      // 执行中：追加消息到正在执行的 turn
      try {
        await api.appendMessage(inputContent);
        setInputContent('');
      } catch (e) {
        console.error('追加消息失败:', e);
      }
    }
  };

  const handleCancel = () => { cancelTurn(); };

  const handleChangeCwd = async () => {
    try {
      const selected = await open({ directory: true, multiple: false, defaultPath: sessionCwd || undefined, title: '选择工作目录' });
      if (selected && typeof selected === 'string') { await setSessionCwd(selected); }
    } catch (error) { console.error('选择目录失败:', error); }
  };

  // ===== 语音模式相关 =====
  const startVoiceRecording = useCallback(async () => {
    if (isRecordingRef.current || !isIdle) return;
    isRecordingRef.current = true;
    setVoiceCancelled(false);
    setVoiceTooShort(false);
    try {
      await recording.startRecording();
    } catch (e: any) {
      isRecordingRef.current = false;
      alert(e.message || "录音启动失败");
    }
  }, [recording, isIdle]);

  const stopVoiceAndSend = useCallback(async () => {
    if (!isRecordingRef.current) return;
    isRecordingRef.current = false;

    // 误触保护：录音不足 1 秒则丢弃
    const elapsedMs = recording.getElapsedMs();
    if (elapsedMs < 1000) {
      recording.cancelRecording();
      setVoiceTooShort(true);
      setTimeout(() => setVoiceTooShort(false), 1500);
      return;
    }

    const voiceDuration = Math.round(elapsedMs / 1000); // 录音时长（秒）
    recording.setState("transcribing");
    try {
      const { blob, mimeType } = await recording.stopRecording();
      const arrayBuffer = await blob.arrayBuffer();
      const bytes = new Uint8Array(arrayBuffer);
      let binary = "";
      for (let i = 0; i < bytes.length; i++) {
        binary += String.fromCharCode(bytes[i]);
      }
      const base64 = btoa(binary);

      const result = await api.transcribeSpeech(base64, mimeType);
      const text = result.text.trim();
      if (text) {
        const audioPath = result.audio_path;
        // 优先用 API 返回的时长，否则用前端录音计时
        const audioDuration = result.duration || voiceDuration;

        sendMessage(text);

        // 轮询等待消息出现后，通过内容匹配关联语音
        const tryAssociate = (retries: number) => {
          const msgs = useStore.getState().messages;
          // 从后往前找内容匹配的 User 消息
          const matched = [...msgs].reverse().find(
            m => m.role === 'User' && m.content === text
          );
          if (matched && !useStore.getState().voiceMessages[matched.id]) {
            console.log("关联语音消息:", matched.id, "->", audioPath);
            addVoiceMessage(matched.id, audioPath, audioDuration);
            return;
          }
          if (retries > 0) {
            setTimeout(() => tryAssociate(retries - 1), 500);
          }
        };
        setTimeout(() => tryAssociate(20), 300);
      }
    } catch (e: any) {
      console.error("语音识别失败:", e);
      alert(`语音识别失败：${e?.message || e}`);
    } finally {
      recording.setState("idle");
    }
  }, [recording, sendMessage]);

  const cancelVoiceRecording = useCallback(() => {
    if (!isRecordingRef.current) return;
    isRecordingRef.current = false;
    recording.cancelRecording();
    setVoiceCancelled(true);
    setTimeout(() => setVoiceCancelled(false), 1500);
  }, [recording]);

  // 语音模式全局键盘事件（空格键录音）
  useEffect(() => {
    if (!voiceMode || !hasStt) return;

    const handleGlobalKeyDown = (e: globalThis.KeyboardEvent) => {
      if (e.code === 'Space' && !e.repeat && !isRecordingRef.current && isIdle) {
        e.preventDefault();
        startVoiceRecording();
      }
      if (e.key === 'Escape' && isRecordingRef.current) {
        e.preventDefault();
        cancelVoiceRecording();
      }
    };

    const handleGlobalKeyUp = (e: globalThis.KeyboardEvent) => {
      if (e.code === 'Space' && isRecordingRef.current) {
        e.preventDefault();
        stopVoiceAndSend();
      }
    };

    window.addEventListener('keydown', handleGlobalKeyDown);
    window.addEventListener('keyup', handleGlobalKeyUp);
    return () => {
      window.removeEventListener('keydown', handleGlobalKeyDown);
      window.removeEventListener('keyup', handleGlobalKeyUp);
    };
  }, [voiceMode, hasStt, isIdle, startVoiceRecording, stopVoiceAndSend, cancelVoiceRecording]);

  const displayCwd = sessionCwd
    ? sessionCwd.split('/').filter(Boolean).slice(-2).join('/')
    : '';

  // ===== 渲染 =====
  return (
    <div className="p-4 border-t bg-background">
      <div className="max-w-3xl mx-auto">
        {voiceMode && hasStt ? (
          // ===== 语音模式 =====
          <div>
            <div className="relative">
              {recording.state === "transcribing" ? (
                <div className="flex items-center justify-center h-[60px] rounded-md bg-muted/50">
                  <Loader2 className="w-4 h-4 animate-spin mr-2" />
                  <span className="text-sm text-muted-foreground">识别中...</span>
                </div>
              ) : recording.state === "recording" ? (
                <div
                  className="flex flex-col items-center justify-center h-[60px] rounded-md bg-red-500/10 border border-red-500/30"
                  onMouseLeave={cancelVoiceRecording}
                >
                  <div className="flex items-center gap-2">
                    <div className="w-3 h-3 rounded-full bg-red-500 animate-pulse" />
                    <span className="text-sm font-medium">录音中 {recording.duration}s</span>
                  </div>
                  <span className="text-xs text-muted-foreground mt-0.5">松开发送，移出取消</span>
                </div>
              ) : (
                <div className="flex items-center gap-2">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-[60px] w-10 shrink-0 text-muted-foreground hover:text-foreground"
                    onClick={() => setVoiceMode(false)}
                    title="切换到文字模式"
                  >
                    <Keyboard className="w-5 h-5" />
                  </Button>
                  {!isIdle ? (
                    <Button
                      onClick={handleCancel}
                      className="flex-1 h-[60px] rounded-md bg-destructive hover:bg-destructive/90 text-destructive-foreground"
                    >
                      <Square className="w-4 h-4 mr-2" />
                      停止
                    </Button>
                  ) : (
                    <button
                      className="flex-1 h-[60px] rounded-md bg-muted/50 hover:bg-muted border border-border flex items-center justify-center gap-2 text-sm text-muted-foreground hover:text-foreground transition-colors select-none"
                      onMouseDown={(e) => { e.preventDefault(); startVoiceRecording(); }}
                      onMouseUp={stopVoiceAndSend}
                      onContextMenu={(e) => e.preventDefault()}
                    >
                      <Mic className="w-4 h-4" />
                      按住说话 / 按空格说话
                    </button>
                  )}
                </div>
              )}
              {/* 提示信息 */}
              {voiceCancelled && (
                <div className="absolute inset-0 flex items-center justify-center bg-background/90 rounded-md">
                  <span className="text-sm text-muted-foreground">已取消</span>
                </div>
              )}
              {voiceTooShort && (
                <div className="absolute inset-0 flex items-center justify-center bg-background/90 rounded-md">
                  <span className="text-sm text-muted-foreground">说话时间太短</span>
                </div>
              )}
            </div>
            <div className="mt-1.5 flex items-center justify-between text-xs text-muted-foreground">
              <button
                onClick={handleChangeCwd}
                disabled={!isIdle}
                className="flex items-center gap-1 hover:text-foreground transition-colors truncate max-w-[300px] disabled:opacity-50 disabled:cursor-default disabled:hover:text-muted-foreground"
                title={sessionCwd || '点击设置工作目录'}
              >
                <FolderOpen className="w-3 h-3 shrink-0" />
                <span className="truncate">{displayCwd || '设置工作目录'}</span>
              </button>
              <span>空格键 录音</span>
            </div>
          </div>
        ) : (
          // ===== 文字模式 =====
          <div>
            <div className="relative">
              {/* @提及补全下拉列表 */}
              {mentionOpen && filteredCandidates.length > 0 && (
                <div
                  ref={mentionRef}
                  className="absolute bottom-full left-0 mb-1 w-72 max-h-48 overflow-y-auto rounded-md border bg-popover shadow-lg z-50"
                >
                  {filteredCandidates.map((c, i) => (
                    <button
                      key={c.value}
                      className={`w-full flex items-center gap-2 px-3 py-1.5 text-sm text-left hover:bg-accent transition-colors ${
                        i === mentionIndex ? 'bg-accent' : ''
                      }`}
                      onMouseDown={(e) => { e.preventDefault(); selectCandidate(c); }}
                      onMouseEnter={() => setMentionIndex(i)}
                    >
                      {c.kind === 'skill' ? (
                        <Wrench className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      ) : (
                        <Cpu className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      )}
                      <div className="flex-1 min-w-0">
                        <span className="font-medium">{c.label}</span>
                        <span className="ml-2 text-muted-foreground text-xs truncate">{c.hint}</span>
                      </div>
                    </button>
                  ))}
                </div>
              )}

              <Textarea
                ref={textareaRef}
                value={inputContent}
                onChange={(e) => handleInputChange(e.target.value)}
                onKeyDown={handleKeyDown}
                onCompositionStart={() => setIsComposing(true)}
                onCompositionEnd={() => setIsComposing(false)}
                onBlur={() => setTimeout(() => setMentionOpen(false), 150)}
                placeholder={
                  isIdle
                    ? '输入消息... (⌘+Enter 发送，@ 引用 Skill/MCP)'
                    : '追加指示... (⌘+Enter 发送)'
                }
                className="min-h-[60px] max-h-[200px] resize-none pr-24 bg-muted/50 focus-visible:ring-ring"
              />
              {/* 按钮区域 */}
              <div className="absolute right-2 bottom-2 flex items-center gap-1">
                {hasStt && isIdle && (
                  <Button
                    onClick={() => setVoiceMode(true)}
                    size="icon"
                    variant="ghost"
                    className="h-8 w-8 rounded-md text-muted-foreground hover:text-foreground"
                    title="切换到语音模式"
                  >
                    <Mic className="w-4 h-4" />
                  </Button>
                )}
                {!isIdle && (
                  <Button
                    onClick={handleCancel}
                    size="icon"
                    variant="ghost"
                    className="h-8 w-8 rounded-md text-destructive hover:bg-destructive/10"
                    title="取消执行"
                  >
                    <Square className="w-4 h-4" />
                  </Button>
                )}
                <Button
                  onClick={handleSend}
                  disabled={!canSend}
                  size="icon"
                  className={`h-8 w-8 rounded-md ${
                    canSend
                      ? isIdle
                        ? 'bg-green-600 hover:bg-green-700 text-white'
                        : 'bg-blue-600 hover:bg-blue-700 text-white'
                      : 'bg-muted text-muted-foreground'
                  }`}
                  title={isIdle ? '发送消息' : '追加指示'}
                >
                  {isIdle ? (
                    <Send className="w-4 h-4" />
                  ) : (
                    <MessageSquarePlus className="w-4 h-4" />
                  )}
                </Button>
              </div>
            </div>
            <div className="mt-1.5 flex items-center justify-between text-xs text-muted-foreground">
              <button
                onClick={handleChangeCwd}
                disabled={!isIdle}
                className="flex items-center gap-1 hover:text-foreground transition-colors truncate max-w-[300px] disabled:opacity-50 disabled:cursor-default disabled:hover:text-muted-foreground"
                title={sessionCwd || '点击设置工作目录'}
              >
                <FolderOpen className="w-3 h-3 shrink-0" />
                <span className="truncate">{displayCwd || '设置工作目录'}</span>
              </button>
              <span>⌘+Enter 发送</span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
