import { useState, KeyboardEvent, ClipboardEvent, DragEvent, useEffect, useRef, useCallback } from 'react';
import { useStore } from '@/store/useStore';
import { Textarea } from './ui/textarea';
import { Button } from './ui/button';
import { Send, Square, FolderOpen, Wrench, Cpu, Mic, Loader2, Keyboard, MessageSquarePlus, ShieldCheck, ShieldOff, Circle, Paperclip, X } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { convertFileSrc } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DragDropEvent } from '@tauri-apps/api/webview';
import { api, type MediaAsset } from '@/api/tauri';
import { useAudioRecording } from '@/hooks/useAudioRecording';

const MAX_ATTACHMENT_BASE64_BYTES = 50 * 1024 * 1024;

interface MentionCandidate {
  value: string;
  label: string;
  kind: string;
  hint: string;
}

type Attachment = {
  kind: 'image' | 'file';
  url: string;
  title: string;
  mime_type?: string;
};

function imageMimeType(path: string): string | undefined {
  const lower = path.toLowerCase();
  if (lower.endsWith('.jpg') || lower.endsWith('.jpeg')) return 'image/jpeg';
  if (lower.endsWith('.webp')) return 'image/webp';
  if (lower.endsWith('.gif')) return 'image/gif';
  if (lower.endsWith('.png')) return 'image/png';
  return undefined;
}

function fileMimeType(path: string): string | undefined {
  const lower = path.toLowerCase();
  const imageMime = imageMimeType(lower);
  if (imageMime) return imageMime;
  if (lower.endsWith('.pdf')) return 'application/pdf';
  if (lower.endsWith('.txt')) return 'text/plain';
  if (lower.endsWith('.md') || lower.endsWith('.markdown')) return 'text/markdown';
  if (lower.endsWith('.json')) return 'application/json';
  if (lower.endsWith('.csv')) return 'text/csv';
  return undefined;
}

function imageExtFromMime(mimeType: string): string {
  if (mimeType === 'image/jpeg' || mimeType === 'image/jpg') return 'jpg';
  if (mimeType === 'image/webp') return 'webp';
  if (mimeType === 'image/gif') return 'gif';
  return 'png';
}

function resolveAttachmentUrl(url: string): string {
  if (!url) return '';
  if (url.startsWith('http://') || url.startsWith('https://') || url.startsWith('asset://')) {
    return url;
  }
  if (url.startsWith('/')) {
    return convertFileSrc(url);
  }
  return url;
}

function clipboardImagePaths(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map(part => part.trim())
    .map(part => part.replace(/^["']|["']$/g, ''))
    .map(part => {
      if (!part.startsWith('file://')) return part;
      try {
        return decodeURIComponent(part.replace(/^file:\/\//, ''));
      } catch {
        return part.replace(/^file:\/\//, '');
      }
    })
    .filter(part => !!part && !!imageMimeType(part));
}

function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error || new Error('读取附件失败'));
    reader.readAsDataURL(file);
  });
}

function attachmentFromPath(path: string): Attachment {
  const lower = path.toLowerCase();
  const isImage = /\.(png|jpe?g|webp|gif)$/.test(lower);
  return {
    kind: isImage ? 'image' : 'file',
    url: path,
    title: path.split('/').pop() || path,
    mime_type: fileMimeType(path),
  };
}

function base64SizeFromDataUrl(dataUrl: string): number {
  return dataUrl.split(',', 2)[1]?.length ?? 0;
}

function mimeTypeFromDataUrl(dataUrl: string): string | undefined {
  const header = dataUrl.split(',', 1)[0] || '';
  const mime = header.match(/^data:([^;]+);/)?.[1];
  return mime || undefined;
}

function assertBase64Size(dataUrl: string, title: string) {
  if (base64SizeFromDataUrl(dataUrl) > MAX_ATTACHMENT_BASE64_BYTES) {
    throw new Error(`附件“${title}”超过 50MB，已停止发送。`);
  }
}

function estimatedBase64Size(rawBytes: number): number {
  return Math.ceil(rawBytes / 3) * 4;
}

export function MessageInput() {
  const { inputContent, setInputContent, sendMessage, cancelTurn, runStatus, runSummary, isDraft, activeSessionId, sessionRunStatuses, sessionCwd, setSessionCwd, addVoiceMessage, lastDurationMs, lastUsage } = useStore();
  const [isComposing, setIsComposing] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const inputAreaRef = useRef<HTMLDivElement>(null);
  const lastNativeDropAtRef = useRef(0);

  // @提及补全状态
  const [mentionOpen, setMentionOpen] = useState(false);
  const [mentionCandidates, setMentionCandidates] = useState<MentionCandidate[]>([]);
  const [mentionFilter, setMentionFilter] = useState('');
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionStart, setMentionStart] = useState(-1);
  const mentionRef = useRef<HTMLDivElement>(null);

  // 信任模式
  const [trustMode, setTrustMode] = useState('full_trust');
  const [hasMultimodal, setHasMultimodal] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [isDraggingFiles, setIsDraggingFiles] = useState(false);

  const currentRunStatus = isDraft
    ? 'idle'
    : (activeSessionId && sessionRunStatuses[activeSessionId]) || runStatus;
  const sessionTotalTokens = lastUsage?.total_tokens ?? 0;

  useEffect(() => {
    api.getTrustMode().then(setTrustMode).catch(() => {});
    api.hasModelCapability('multimodal').then(setHasMultimodal).catch(() => setHasMultimodal(false));
  }, []);

  useEffect(() => {
    if (!hasMultimodal) {
      setAttachments([]);
      setIsDraggingFiles(false);
    }
  }, [hasMultimodal]);

  const toggleTrustMode = async () => {
    const newMode = trustMode === 'full_trust' ? 'supervised' : 'full_trust';
    try {
      await api.setTrustMode(newMode);
      setTrustMode(newMode);
    } catch (e) {
      console.error('切换信任模式失败:', e);
    }
  };

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
  const canSend = inputContent.trim().length > 0 || (hasMultimodal && attachments.length > 0);  // 执行中也允许输入
  const isTextDropTargetActive = !voiceMode && hasMultimodal && isIdle;

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

  const addAttachments = useCallback((items: Attachment[]) => {
    if (items.length === 0) return;
    setAttachments(prev => {
      const next = [...prev];
      for (const item of items) {
        if (!next.some(existing => existing.url === item.url)) {
          next.push(item);
        }
      }
      return next;
    });
  }, []);

  const addAttachmentsFromPaths = useCallback((paths: string[]) => {
    addAttachments(paths.map(attachmentFromPath));
  }, [addAttachments]);

  const attachmentToBase64Media = async (item: Attachment): Promise<MediaAsset> => {
    if (item.url.startsWith('data:')) {
      assertBase64Size(item.url, item.title);
      return {
        kind: item.kind,
        url: item.url,
        title: item.title,
        mime_type: item.mime_type || mimeTypeFromDataUrl(item.url),
        capability: 'multimodal',
      };
    }

    const encoded = await api.readAttachmentAsDataUrl(item.url, MAX_ATTACHMENT_BASE64_BYTES);
    return {
      kind: item.kind,
      url: encoded.data_url,
      title: item.title || encoded.title,
      mime_type: item.mime_type || encoded.mime_type,
      capability: 'multimodal',
    };
  };

  const attachmentsToBase64Media = async (): Promise<MediaAsset[]> => {
    const media: MediaAsset[] = [];
    for (const item of attachments) {
      media.push(await attachmentToBase64Media(item));
    }
    return media;
  };

  useEffect(() => {
    if (!isTextDropTargetActive) {
      setIsDraggingFiles(false);
      return;
    }

    let disposed = false;
    let unlisten: (() => void) | undefined;

    getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload as DragDropEvent;
      if (payload.type === 'leave') {
        setIsDraggingFiles(false);
        return;
      }
      if (payload.type === 'enter' || payload.type === 'over') {
        setIsDraggingFiles(true);
        return;
      }
      if (payload.type === 'drop') {
        setIsDraggingFiles(false);
        if (payload.paths.length > 0) {
          lastNativeDropAtRef.current = Date.now();
          addAttachmentsFromPaths(payload.paths);
          textareaRef.current?.focus();
        }
      }
    }).then((stopListening) => {
      if (disposed) {
        stopListening();
      } else {
        unlisten = stopListening;
      }
    }).catch((err) => {
      console.error('监听文件拖放失败:', err);
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [addAttachmentsFromPaths, isTextDropTargetActive]);

  const filesToAttachments = async (files: File[]) => {
    const items = await Promise.all(files.map(async (file, index): Promise<Attachment> => {
      const title = file.name || `dropped-file-${index + 1}`;
      if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
        throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
      }
      const fileWithPath = file as File & { path?: string };
      if (fileWithPath.path) {
        return attachmentFromPath(fileWithPath.path);
      }
      if (file.type.startsWith('image/')) {
        const mimeType = file.type || 'image/png';
        return {
          kind: 'image',
          url: await fileToDataUrl(file),
          title: file.name || `dropped-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`,
          mime_type: mimeType,
        };
      }
      return {
        kind: 'file',
        url: await fileToDataUrl(file),
        title,
        mime_type: file.type || 'application/octet-stream',
      };
    }));
    addAttachments(items);
  };

  const handleDragOver = (e: DragEvent<HTMLDivElement>) => {
    if (!hasMultimodal || !isIdle) return;
    if (Array.from(e.dataTransfer.types).includes('Files')) {
      e.preventDefault();
      e.dataTransfer.dropEffect = 'copy';
      setIsDraggingFiles(true);
    }
  };

  const handleDragLeave = (e: DragEvent<HTMLDivElement>) => {
    if (!inputAreaRef.current?.contains(e.relatedTarget as Node | null)) {
      setIsDraggingFiles(false);
    }
  };

  const handleDrop = async (e: DragEvent<HTMLDivElement>) => {
    if (!hasMultimodal || !isIdle) return;
    const files = Array.from(e.dataTransfer.files);
    if (files.length === 0) return;
    e.preventDefault();
    setIsDraggingFiles(false);
    if (Date.now() - lastNativeDropAtRef.current < 500) return;
    try {
      await filesToAttachments(files);
      textareaRef.current?.focus();
    } catch (err) {
      console.error('读取拖放文件失败:', err);
      alert(err instanceof Error ? err.message : '读取拖放文件失败');
    }
  };

  const handlePaste = async (e: ClipboardEvent<HTMLTextAreaElement>) => {
    if (!hasMultimodal || !isIdle) return;

    const files = Array.from(e.clipboardData.files).filter(file =>
      file.type.startsWith('image/')
    );
    if (files.length > 0) {
      e.preventDefault();
      try {
        const pasted = await Promise.all(files.map(async (file, index) => {
          const mimeType = file.type || 'image/png';
          const title = file.name || `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`;
          if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
            throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
          }
          return {
            kind: 'image' as const,
            url: await fileToDataUrl(file),
            title,
            mime_type: mimeType,
          };
        }));
        addAttachments(pasted);
      } catch (err) {
        console.error('读取粘贴图片失败:', err);
        alert(err instanceof Error ? err.message : '读取粘贴图片失败');
      }
      return;
    }

    const imageItems = Array.from(e.clipboardData.items).filter(item =>
      item.kind === 'file' && item.type.startsWith('image/')
    );
    if (imageItems.length > 0) {
      e.preventDefault();
      try {
        const pasted = await Promise.all(imageItems.map(async (item, index): Promise<Attachment | null> => {
          const file = item.getAsFile();
          if (!file) return null;
          const mimeType = file.type || item.type || 'image/png';
          const title = file.name || `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`;
          if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
            throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
          }
          return {
            kind: 'image' as const,
            url: await fileToDataUrl(file),
            title,
            mime_type: mimeType,
          };
        }));
        addAttachments(pasted.filter((item): item is Attachment => item !== null));
      } catch (err) {
        console.error('读取粘贴图片失败:', err);
        alert(err instanceof Error ? err.message : '读取粘贴图片失败');
      }
      return;
    }

    const text = e.clipboardData.getData('text/plain');
    const paths = clipboardImagePaths(text);
    const nonEmptyLines = text
      .split(/\r?\n/)
      .map(line => line.trim())
      .filter(Boolean);
    if (paths.length > 0 && paths.length === nonEmptyLines.length) {
      e.preventDefault();
      addAttachments(paths.map(path => ({
        kind: 'image',
        url: path,
        title: path.split('/').pop() || path,
        mime_type: imageMimeType(path),
      })));
    }
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
    if (attachments.length > 0 && !hasMultimodal) {
      setAttachments([]);
      alert('未配置多模态模型，文件上传能力已关闭。');
      return;
    }
    let media: MediaAsset[];
    try {
      media = await attachmentsToBase64Media();
    } catch (err) {
      console.error('附件转换失败:', err);
      alert(err instanceof Error ? err.message : '附件转换失败');
      return;
    }
    const content = inputContent.trim() || (attachments.length > 0 ? '请处理这些附件。' : inputContent);
    if (isIdle) {
      await sendMessage(content, media);
      setAttachments([]);
    } else {
      // 执行中：追加消息到正在执行的 turn
      if (!activeSessionId) return;
      try {
        const appended = await api.appendMessage(activeSessionId, content);
        if (appended) {
          setInputContent('');
          setAttachments([]);
        } else {
          console.warn('当前会话没有正在执行的任务，追加消息未发送');
        }
      } catch (e) {
        console.error('追加消息失败:', e);
      }
    }
  };

  const handleCancel = () => { cancelTurn(); };

  const handleAttachFiles = async () => {
    if (!hasMultimodal) return;
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: '选择图片或文件',
        filters: [
          { name: '图片和文件', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'pdf', 'txt', 'md', 'json', 'csv'] },
        ],
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (paths.length === 0) return;
      addAttachmentsFromPaths(paths);
    } catch (e) {
      console.error('选择附件失败:', e);
    }
  };

  const removeAttachment = (url: string) => {
    setAttachments(prev => prev.filter(item => item.url !== url));
  };

  const handleChangeCwd = async () => {
    try {
      const selected = await open({ directory: true, multiple: false, defaultPath: sessionCwd || undefined, title: '选择对话目录' });
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
          // 从后往前找内容匹配的 user 消息
          const matched = [...msgs].reverse().find(
            m => m.role === 'user' && m.content === text
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
                title={sessionCwd || '点击设置对话目录'}
              >
                <FolderOpen className="w-3 h-3 shrink-0" />
                <span className="truncate">{displayCwd || '设置对话目录'}</span>
              </button>
              <span>空格键 录音</span>
            </div>
          </div>
        ) : (
          // ===== 文字模式 =====
          <div>
            <div
              ref={inputAreaRef}
              className="relative"
              onDragOver={handleDragOver}
              onDragLeave={handleDragLeave}
              onDrop={handleDrop}
            >
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

              {attachments.length > 0 && (
                <div className="mb-2 flex flex-wrap gap-1.5">
                  {attachments.map(item => (
                    <span
                      key={item.url}
                      className="inline-flex h-9 max-w-[260px] items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs"
                      title={item.url}
                    >
                      {item.kind === 'image' ? (
                        <img
                          src={resolveAttachmentUrl(item.url)}
                          alt={item.title}
                          className="h-6 w-6 shrink-0 rounded object-cover"
                        />
                      ) : (
                        <Paperclip className="h-3 w-3 shrink-0" />
                      )}
                      <span className="truncate">{item.title}</span>
                      <button
                        type="button"
                        onClick={() => removeAttachment(item.url)}
                        className="ml-1 text-muted-foreground hover:text-foreground"
                        title="移除附件"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </span>
                  ))}
                </div>
              )}

              <Textarea
                ref={textareaRef}
                value={inputContent}
                onChange={(e) => handleInputChange(e.target.value)}
                onKeyDown={handleKeyDown}
                onPaste={handlePaste}
                onCompositionStart={() => setIsComposing(true)}
                onCompositionEnd={() => setIsComposing(false)}
                onBlur={() => setTimeout(() => setMentionOpen(false), 150)}
                placeholder={
                  isIdle
                    ? '输入消息... (⌘+Enter 发送，@ 引用 Skill/MCP)'
                    : '追加指示... (⌘+Enter 发送)'
                }
                className="min-h-[60px] max-h-[200px] resize-none pr-32 bg-muted/50 focus-visible:ring-ring"
              />
              {isDraggingFiles && (
                <div className="pointer-events-none absolute inset-0 flex items-center justify-center rounded-md border border-dashed border-primary bg-background/80 text-sm text-primary">
                  松开添加文件
                </div>
              )}
              {/* 按钮区域 */}
              <div className="absolute right-2 bottom-2 flex items-center gap-1">
                {hasMultimodal && isIdle && (
                  <Button
                    onClick={handleAttachFiles}
                    size="icon"
                    variant="ghost"
                    className="h-8 w-8 rounded-md text-muted-foreground hover:text-foreground"
                    title="添加图片或文件"
                  >
                    <Paperclip className="w-4 h-4" />
                  </Button>
                )}
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
              <div className="flex items-center gap-3 min-w-0">
                <button
                  onClick={handleChangeCwd}
                  disabled={!isIdle}
                  className="flex items-center gap-1 hover:text-foreground transition-colors truncate max-w-[300px] disabled:opacity-50 disabled:cursor-default disabled:hover:text-muted-foreground shrink-0"
                  title={sessionCwd || '点击设置对话目录'}
                >
                  <FolderOpen className="w-3 h-3 shrink-0" />
                  <span className="truncate">{displayCwd || '设置对话目录'}</span>
                </button>
                {currentRunStatus !== 'idle' ? (
                  <span className="flex items-center gap-1 text-yellow-500 truncate" title={runSummary || '执行中'}>
                    <Circle className="w-1.5 h-1.5 animate-pulse shrink-0" />
                    <span className="truncate">{runSummary && runSummary.length > 30 ? runSummary.slice(0, 30) + '...' : (runSummary || '执行中')}</span>
                  </span>
                ) : (
                  <span className="flex items-center gap-1 text-green-500">
                    <Circle className="w-1.5 h-1.5 fill-current shrink-0" />
                    <span>就绪</span>
                  </span>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {sessionTotalTokens > 0 && (
                  <span className="text-muted-foreground/50 tabular-nums">
                    {lastDurationMs ? `${(lastDurationMs / 1000).toFixed(1)}s · ` : ''}
                    {sessionTotalTokens.toLocaleString()} tokens
                    {false && (
                      <span className="text-blue-400 ml-1">placeholder</span>
                    )}
                  </span>
                )}
                <button
                  onClick={toggleTrustMode}
                  className={`flex items-center gap-1 transition-colors ${
                    trustMode === 'supervised'
                      ? 'text-amber-500 hover:text-amber-400'
                      : 'hover:text-foreground'
                  }`}
                  title={trustMode === 'supervised' ? '监督模式（高风险操作需确认）' : '完全信任模式（自动执行）'}
                >
                  {trustMode === 'supervised' ? (
                    <><ShieldCheck className="w-3 h-3" /><span>监督</span></>
                  ) : (
                    <><ShieldOff className="w-3 h-3" /><span>信任</span></>
                  )}
                </button>
                <span>⌘+Enter 发送</span>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
