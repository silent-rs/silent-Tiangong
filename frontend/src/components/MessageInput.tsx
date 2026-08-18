import { useState, KeyboardEvent, ClipboardEvent, DragEvent, useEffect, useRef, useCallback } from 'react';
import type { SetStateAction } from 'react';
import { selectCurrentInputCacheKey, selectCurrentInputCache, useStore } from '@/store/useStore';
import { MentionEditor, type MentionEditorHandle } from './MentionEditor';
import { Button } from './ui/button';
import { Send, Square, FolderOpen, Wrench, Cpu, Mic, Loader2, Keyboard, MessageSquarePlus, ShieldCheck, ShieldOff, Circle, Paperclip, X, Users, Brain, Clock } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { DragDropEvent } from '@tauri-apps/api/webview';
import { api, textContent } from '@/api/tauri';
import { useAudioRecording } from '@/hooks/useAudioRecording';
import {
  type Attachment,
  MAX_ATTACHMENT_BASE64_BYTES,
  attachmentKindFromMime,
  imageMimeType,
  imageExtFromMime,
  clipboardImagePaths,
  fileToDataUrl,
  attachmentFromPath,
  estimatedBase64Size,
  resolveAttachmentUrl,
} from '@/utils/attachments';
import { replaceMentionCompletion } from '@/utils/mentionEditorModel';
import { formatDuration } from './message/utils';
import { SessionInputPluginHost } from './SessionInputPluginHost';

interface MentionCandidate {
  value: string;
  label: string;
  kind: string;
  hint: string;
}

const SLASH_COMMANDS: MentionCandidate[] = [
  {
    value: '/压缩对话',
    label: '/压缩对话',
    kind: 'command',
    hint: '压缩早期上下文',
  },
  {
    value: '/清理对话',
    label: '/清理对话',
    kind: 'command',
    hint: '清理当前上下文',
  },
];

export function MessageInput() {
  const cacheKey = useStore(selectCurrentInputCacheKey);
  const inputCache = useStore(selectCurrentInputCache);
  const inputContent = inputCache.text;
  const attachments = inputCache.attachments;
  const isSending = inputCache.is_sending;
  const setInputCacheText = useStore((state) => state.setInputCacheText);
  const setInputCacheAttachments = useStore((state) => state.setInputCacheAttachments);
  const sendMessage = useStore((state) => state.sendMessage);
  const appendMessage = useStore((state) => state.appendMessage);
  const cancelTurn = useStore((state) => state.cancelTurn);
  const beginContextManagement = useStore((state) => state.beginContextManagement);
  const endContextManagement = useStore((state) => state.endContextManagement);
  const runStatus = useStore((state) => state.runStatus);
  const runSummary = useStore((state) => state.runSummary);
  const lastDurationMs = useStore((state) => state.lastDurationMs);
  const isNewConversation = useStore((state) => state.isNewConversation);
  const activeSessionId = useStore((state) => state.activeSessionId);
  const currentSessionRunStatus = useStore((state) => (
    state.activeSessionId ? state.sessionRunStatuses[state.activeSessionId] : undefined
  ));
  const sessionCwd = useStore((state) => state.sessionCwd);
  const setSessionCwd = useStore((state) => state.setSessionCwd);
  const addVoiceMessage = useStore((state) => state.addVoiceMessage);
  const lastUsage = useStore((state) => state.lastUsage);
  const tokenStats = useStore((state) => state.tokenStats);
  const agents = useStore((state) => state.agents);
  const selectedAgentTab = useStore((state) => state.selectedAgentTab);
  const reasoningEffort = useStore((state) => state.reasoningEffort);
  const setReasoningEffort = useStore((state) => state.setReasoningEffort);
  const isComposingRef = useRef(false);
  const editorRef = useRef<MentionEditorHandle>(null);
  const inputAreaRef = useRef<HTMLDivElement>(null);
  const lastNativeDropAtRef = useRef(0);

  // @提及补全状态
  const [mentionOpen, setMentionOpen] = useState(false);
  const [mentionCandidates, setMentionCandidates] = useState<MentionCandidate[]>([]);
  const [mentionFilter, setMentionFilter] = useState('');
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionStart, setMentionStart] = useState(-1);
  const [completionMode, setCompletionMode] = useState<'mention' | 'slash'>('mention');
  const mentionRef = useRef<HTMLDivElement>(null);
  const candidateRefs = useRef<Array<HTMLButtonElement | null>>([]);

  // 信任模式
  const [trustMode, setTrustMode] = useState('full_trust');
  const [isDraggingFiles, setIsDraggingFiles] = useState(false);

  const setInputContent = useCallback((content: string) => {
    if (cacheKey) setInputCacheText(cacheKey, content);
  }, [cacheKey, setInputCacheText]);

  const setAttachments = useCallback((update: SetStateAction<Attachment[]>) => {
    if (!cacheKey) return;
    const current = useStore.getState().inputCaches[cacheKey]?.attachments ?? [];
    const next = typeof update === 'function' ? update(current) : update;
    setInputCacheAttachments(cacheKey, next);
  }, [cacheKey, setInputCacheAttachments]);

  const currentRunStatus = isNewConversation
    ? 'idle'
    : currentSessionRunStatus || runStatus;
  const selectedAgent = selectedAgentTab
    ? agents.find((agent) => agent.role === selectedAgentTab)
    : null;
  const selectedAgentId = selectedAgent?.agentId ?? null;
  const displayTokens = selectedAgentId
    ? (tokenStats?.agent_current_tokens?.[selectedAgentId] ?? 0)
    : (tokenStats?.current_tokens ?? 0);
  const compressionThreshold = tokenStats?.compression_threshold_tokens ?? 0;
  const compressionProgress = compressionThreshold > 0
    ? Math.min(100, Math.round((displayTokens / compressionThreshold) * 100))
    : 0;
  const selectedAgentTotalTokens = selectedAgentId
    ? (tokenStats?.agent_token_usage?.[selectedAgentId]?.total_tokens ?? 0)
    : 0;
  const totalTokens = selectedAgentId
    ? (selectedAgentTotalTokens || displayTokens)
    : (tokenStats?.total_tokens ?? lastUsage?.total_tokens ?? 0);
  const activeAgentLabel = selectedAgent?.label ?? null;

  useEffect(() => {
    let cancelled = false;
    const loadTrustMode = activeSessionId
      ? api.getTrustMode(activeSessionId)
      : api.getDefaultTrustMode();
    loadTrustMode
      .then((mode) => {
        if (!cancelled) setTrustMode(mode);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [activeSessionId]);

  const toggleTrustMode = async () => {
    const newMode = trustMode === 'full_trust' ? 'supervised' : 'full_trust';
    try {
      if (activeSessionId) {
        await api.setTrustMode(newMode, activeSessionId);
      }
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
  const currentSessionStatus = isNewConversation
    ? 'idle'
    : currentSessionRunStatus || runStatus;
  const isIdle = currentSessionStatus === 'idle';
  const canSend = !isSending
    && !!cacheKey
    && (inputContent.trim().length > 0 || attachments.length > 0);
  const isTextDropTargetActive = !voiceMode && !!cacheKey;

  // 运行中实时计时：维护单调递增的显示基准（baseMs@baseAt），事件到达与本地
  // tick 都只向前推进——事件值与外推值取大，杜绝显示回跳；TurnElapsed 事件
  // 稀疏（如工具执行阶段）时秒数依然持续跳动。
  const liveTimerRef = useRef<{ baseMs: number; baseAt: number }>({ baseMs: 0, baseAt: 0 });
  const [durationTick, setDurationTick] = useState(0);
  useEffect(() => {
    if (isIdle) return;
    const timer = setInterval(() => setDurationTick((tick) => tick + 1), 1000);
    return () => clearInterval(timer);
  }, [isIdle]);
  const liveDurationLabel = (() => {
    void durationTick;
    const { baseMs, baseAt } = liveTimerRef.current;
    if (isIdle || lastDurationMs == null) {
      // 空闲或新一轮尚未收到首个计时事件：复位基准。
      liveTimerRef.current = { baseMs: 0, baseAt: 0 };
      return '';
    }
    const now = Date.now();
    const extrapolated = baseMs > 0 ? baseMs + (now - baseAt) : 0;
    const candidate = Math.max(lastDurationMs, extrapolated);
    liveTimerRef.current = { baseMs: candidate, baseAt: now };
    return candidate < 1000 ? '' : formatDuration(candidate);
  })();
  // 自动调整文本框高度（MentionEditor 内部按 value 自适应，这里不再单独维护）

  // ===== 文字模式相关 =====
  const loadCandidates = useCallback(async () => {
    try {
      const candidates = await api.getMentionCandidates();
      setMentionCandidates(candidates);
    } catch (e) {
      console.error('加载提及候选失败:', e);
    }
  }, []);

  const filteredCandidates = (() => {
    if (completionMode === 'slash') {
      const filter = mentionFilter.toLowerCase();
      if (!filter) return SLASH_COMMANDS;
      return SLASH_COMMANDS.filter(c => c.value.toLowerCase().startsWith(filter));
    }

    // 合并 API 候选和 Agent 候选
    const aliveAgents = agents.filter(a => a.status !== 'terminated');
    const agentCandidates: MentionCandidate[] = aliveAgents.map(a => ({
      value: `@${a.role}`,
      label: a.label,
      kind: 'agent',
      hint: `Agent · ${a.status === 'running' ? '执行中' : a.status === 'idle' ? '空闲' : a.status === 'waiting_for_lock' ? '等待文件锁' : a.status === 'waiting_for_user' ? '等待用户' : '错误'}`,
    }));
    // 当存在活跃 Agent 时添加 @all 广播选项
    if (aliveAgents.length > 0) {
      agentCandidates.push({
        value: '@all',
        label: 'All',
        kind: 'agent',
        hint: `广播给全部 ${aliveAgents.length} 个 Agent`,
      });
    }
    const all = [...agentCandidates, ...mentionCandidates];
    if (!mentionFilter) return all;
    const lower = mentionFilter.toLowerCase();
    return all.filter(c =>
      c.label.toLowerCase().includes(lower)
      || c.value.toLowerCase().includes(lower)
      || c.hint.toLowerCase().includes(lower)
    );
  })();

  useEffect(() => {
    if (!mentionOpen) return;
    candidateRefs.current[mentionIndex]?.scrollIntoView({
      block: 'nearest',
    });
  }, [mentionIndex, mentionOpen, filteredCandidates.length]);

  const executeSlashCommand = useCallback(async (command: string) => {
    const trimmed = command.trim();
    if (trimmed === '/压缩对话' || trimmed === '/compress') {
      setInputContent('');
      beginContextManagement('正在压缩早期上下文...');
      try {
        const ok = await api.compressContext();
        if (!ok) {
          endContextManagement();
          alert('压缩对话没有执行成功，请稍后重试。');
        }
      } catch (e) {
        endContextManagement();
        console.error('压缩对话失败:', e);
        alert(e instanceof Error ? e.message : '压缩对话失败');
      }
      return true;
    }
    if (trimmed === '/清理对话' || trimmed === '/reset') {
      setInputContent('');
      beginContextManagement('正在清理上下文...');
      try {
        const ok = await api.resetContext();
        if (!ok) {
          endContextManagement();
          alert('清理对话没有执行成功，请稍后重试。');
        }
      } catch (e) {
        endContextManagement();
        console.error('清理对话失败:', e);
        alert(e instanceof Error ? e.message : '清理对话失败');
      }
      return true;
    }
    return false;
  }, [beginContextManagement, endContextManagement, setInputContent]);

  const handleInputChange = (value: string) => {
    setInputContent(value);
    const cursorPos = editorRef.current?.getSelection()?.start ?? value.length;
    const beforeCursor = value.slice(0, cursorPos);
    if (beforeCursor.startsWith('/') && !/\s/.test(beforeCursor)) {
      setMentionStart(0);
      setMentionFilter(beforeCursor);
      setMentionIndex(0);
      setCompletionMode('slash');
      setMentionOpen(true);
      return;
    }

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
      setCompletionMode('mention');
      if (!mentionOpen) { loadCandidates(); setMentionOpen(true); }
    } else {
      setMentionOpen(false);
    }
  };

  const selectCandidate = (candidate: MentionCandidate) => {
    if (mentionStart < 0) return;
    if (candidate.kind === 'command') {
      setMentionOpen(false);
      void executeSlashCommand(candidate.value);
      return;
    }
    const editor = editorRef.current;
    const cursorPos = editor?.getSelection()?.start ?? inputContent.length;
    const replacement = replaceMentionCompletion(
      inputContent,
      mentionStart,
      cursorPos,
      candidate.value,
    );
    if (!replacement) return;
    setInputContent(replacement.value);
    setMentionOpen(false);
    setTimeout(() => {
      if (editor) {
        editor.focus();
        editor.setSelection(replacement.offset);
      }
    }, 0);
  };

  const addAttachments = useCallback((items: Attachment[]) => {
    if (items.length === 0) return;
    setAttachments(prev => {
      const next = [...prev];
      for (const item of items) {
        if (!next.some(existing => existing.source === item.source)) {
          next.push(item);
        }
      }
      return next;
    });
  }, [setAttachments]);

  const addAttachmentsFromPaths = useCallback((paths: string[]) => {
    addAttachments(paths.map(attachmentFromPath));
  }, [addAttachments]);

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
          editorRef.current?.focus();
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
      const fileWithPath = file as File & { path?: string };
      if (fileWithPath.path) {
        return attachmentFromPath(fileWithPath.path);
      }
      if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
        throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
      }
      const mimeType = file.type || 'application/octet-stream';
      return {
        kind: attachmentKindFromMime(mimeType),
        source: await fileToDataUrl(file),
        original_name: file.name || (
          mimeType.startsWith('image/')
            ? `dropped-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`
            : title
        ),
        mime_type: mimeType,
      };
    }));
    addAttachments(items);
  };

  const handleDragOver = (e: DragEvent<HTMLDivElement>) => {
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
    const files = Array.from(e.dataTransfer.files);
    if (files.length === 0) return;
    e.preventDefault();
    setIsDraggingFiles(false);
    if (Date.now() - lastNativeDropAtRef.current < 500) return;
    try {
      await filesToAttachments(files);
      editorRef.current?.focus();
    } catch (err) {
      console.error('读取拖放文件失败:', err);
      alert(err instanceof Error ? err.message : '读取拖放文件失败');
    }
  };

  const handlePaste = async (e: ClipboardEvent<HTMLDivElement>) => {
    const files = Array.from(e.clipboardData.files);
    if (files.length > 0) {
      e.preventDefault();
      try {
        const pasted = await Promise.all(files.map(async (file, index) => {
          const mimeType = file.type || 'application/octet-stream';
          const title = file.name || (mimeType.startsWith('image/')
            ? `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`
            : `pasted-file-${Date.now()}-${index + 1}`);
          if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
            throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
          }
          return {
            kind: attachmentKindFromMime(mimeType),
            source: await fileToDataUrl(file),
            original_name: title,
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

    const fileItems = Array.from(e.clipboardData.items).filter(item => item.kind === 'file');
    if (fileItems.length > 0) {
      e.preventDefault();
      try {
        const pasted = await Promise.all(fileItems.map(async (item, index): Promise<Attachment | null> => {
          const file = item.getAsFile();
          if (!file) return null;
          const mimeType = file.type || item.type || 'application/octet-stream';
          const title = file.name || (mimeType.startsWith('image/')
            ? `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`
            : `pasted-file-${Date.now()}-${index + 1}`);
          if (estimatedBase64Size(file.size) > MAX_ATTACHMENT_BASE64_BYTES) {
            throw new Error(`附件“${title}”超过 50MB，已停止添加。`);
          }
          return {
            kind: attachmentKindFromMime(mimeType),
            source: await fileToDataUrl(file),
            original_name: title,
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
        source: path,
        original_name: path.split(/[\\/]/).pop() || path,
        mime_type: imageMimeType(path),
      })));
    }
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (mentionOpen && filteredCandidates.length > 0) {
      if (e.key === 'ArrowDown') { e.preventDefault(); setMentionIndex(i => (i + 1) % filteredCandidates.length); return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); setMentionIndex(i => (i - 1 + filteredCandidates.length) % filteredCandidates.length); return; }
      if (e.key === 'Enter' && !e.metaKey && !e.ctrlKey) { e.preventDefault(); selectCandidate(filteredCandidates[mentionIndex]); return; }
      if (e.key === 'Escape') { e.preventDefault(); setMentionOpen(false); return; }
      if (e.key === 'Tab') { e.preventDefault(); selectCandidate(filteredCandidates[mentionIndex]); return; }
    }
    if (e.key === 'Enter' && !e.shiftKey && !isComposingRef.current && !e.nativeEvent.isComposing && e.keyCode !== 229) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleSend = async () => {
    if (!canSend) return;
    setMentionOpen(false);

    // 在任何异步调用前固定目标与完整输入快照，后续切换会话不改变投递目标。
    const targetCacheKey = cacheKey;
    if (!targetCacheKey) return;
    const inputSnapshot = {
      ...inputCache,
      attachments: inputCache.attachments.map((attachment) => ({ ...attachment })),
    };

    // slash command 拦截
    const trimmed = inputSnapshot.text.trim();
    if (await executeSlashCommand(trimmed)) {
      return;
    }
    const content = inputSnapshot.text.trim()
      || (inputSnapshot.attachments.length > 0 ? '请处理这些附件。' : inputSnapshot.text);
    if (isIdle) {
      await sendMessage(
        targetCacheKey,
        content,
        inputSnapshot.attachments,
        inputSnapshot.revision,
        trustMode,
      );
    } else {
      // 执行中：追加消息到正在执行的 turn
      const appended = await appendMessage(
        targetCacheKey,
        content,
        inputSnapshot.attachments,
        inputSnapshot.revision,
      );
      if (!appended) {
        console.warn('当前会话没有正在执行的任务，追加消息未发送');
      }
    }
  };

  const handleCancel = () => { cancelTurn(); };

  const handleAttachFiles = async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: '选择图片或文件',
        filters: [
          {
            name: '图片、音视频和文件',
            extensions: [
              'png', 'jpg', 'jpeg', 'webp', 'gif',
              'mp3', 'wav', 'm4a', 'ogg', 'flac',
              'mp4', 'mov', 'webm', 'mkv',
              'pdf', 'docx', 'xlsx', 'pptx', 'txt', 'md', 'json', 'csv',
            ],
          },
        ],
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (paths.length === 0) return;
      addAttachmentsFromPaths(paths);
    } catch (e) {
      console.error('选择附件失败:', e);
    }
  };

  const removeAttachment = (source: string) => {
    setAttachments(prev => prev.filter(item => item.source !== source));
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
    const targetCacheKey = cacheKey;
    if (!targetCacheKey) {
      recording.cancelRecording();
      return;
    }
    const targetCache = useStore.getState().inputCaches[targetCacheKey];
    if (!targetCache) {
      recording.cancelRecording();
      return;
    }

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

        await sendMessage(targetCacheKey, text, [], targetCache.revision, trustMode);

        // 轮询等待消息出现后，通过内容匹配关联语音
        const tryAssociate = (retries: number) => {
          const msgs = useStore.getState().messages;
          // 从后往前找内容匹配的 user 消息
          const matched = [...msgs].reverse().find(
            m => m.role === 'user' && textContent(m) === text
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
  }, [cacheKey, recording, sendMessage, trustMode]);

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
  const containerRef = useRef<HTMLDivElement>(null);
  const [compact, setCompact] = useState(false);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      setCompact(el.clientWidth < 500);
    });
    observer.observe(el);
    setCompact(el.clientWidth < 500);
    return () => observer.disconnect();
  }, []);

  return (
    <div ref={containerRef} className="p-4 border-t bg-background">
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
            {/* 输入框上方：运行状态 + 思考强度 */}
            <div className="mb-1 flex items-center justify-between text-xs text-muted-foreground">
              <div className="flex items-center gap-2 min-w-0">
                {liveDurationLabel && (
                  <span className="inline-flex items-center gap-0.5 shrink-0 text-blue-500 tabular-nums" title="本轮已用时长">
                    <Clock className="w-3 h-3" />
                    {liveDurationLabel}
                  </span>
                )}
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
                {activeAgentLabel && (
                  <span className="text-muted-foreground/60">[{activeAgentLabel}]</span>
                )}
              </div>
              <div className="flex items-center gap-1 shrink-0">
                <Brain className="w-3 h-3" />
                <select
                  value={reasoningEffort}
                  onChange={(e) => setReasoningEffort(e.target.value)}
                  className="bg-transparent text-xs text-muted-foreground hover:text-foreground cursor-pointer outline-none border-none appearance-none pr-1"
                  title="思考强度"
                >
                  <option value="none">不思考</option>
                  <option value="low">低强度</option>
                  <option value="medium">中强度</option>
                  <option value="high">高强度</option>
                  <option value="max">最大强度</option>
                </select>
              </div>
            </div>
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
                  className="mention-completion-menu absolute bottom-full left-0 z-50 mb-1 max-h-72 w-[min(36rem,calc(100vw-2rem))] overflow-y-auto overflow-x-hidden rounded-md border bg-popover shadow-lg"
                >
                  {filteredCandidates.map((c, i) => (
                    <button
                      key={c.value}
                      ref={(el) => { candidateRefs.current[i] = el; }}
                      className={`flex w-full items-start gap-2 px-3 py-2 text-left text-sm transition-colors hover:bg-accent ${
                        i === mentionIndex ? 'bg-accent' : ''
                      }`}
                      onMouseDown={(e) => { e.preventDefault(); selectCandidate(c); }}
                      onMouseEnter={() => setMentionIndex(i)}
                    >
                      {c.kind === 'skill' ? (
                        <Wrench className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      ) : c.kind === 'agent' ? (
                        <Users className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      ) : c.kind === 'command' ? (
                        <Keyboard className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      ) : (
                        <Cpu className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      )}
                      <div className="min-w-0 flex-1 overflow-hidden">
                        <div className="flex min-w-0 items-baseline gap-2">
                          <span className="truncate font-medium">{c.label}</span>
                          {c.kind === 'skill' && c.value.includes('@') && (
                            <span className="shrink-0 text-xs text-muted-foreground">
                              {c.value.replace(/^@/, '')}
                            </span>
                          )}
                        </div>
                        <span className="mt-0.5 block whitespace-normal break-words text-xs leading-5 text-muted-foreground">
                          {c.hint}
                        </span>
                      </div>
                    </button>
                  ))}
                </div>
              )}

              {attachments.length > 0 && (
                <div className="mb-2 flex flex-wrap gap-1.5">
                  {attachments.map(item => (
                    <span
                      key={(item.original_name ?? '') + item.source.slice(0, 40)}
                      className="inline-flex h-9 max-w-[260px] items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs"
                      title={item.original_name ?? item.source}
                    >
                      {item.kind === 'image' ? (
                        <img
                          src={resolveAttachmentUrl(item.source)}
                          alt={item.original_name ?? '附件'}
                          className="h-6 w-6 shrink-0 rounded object-cover"
                        />
                      ) : (
                        <Paperclip className="h-3 w-3 shrink-0" />
                      )}
                      <span className="truncate">{item.original_name ?? item.source}</span>
                      <button
                        type="button"
                        onClick={() => removeAttachment(item.source)}
                        className="ml-1 text-muted-foreground hover:text-foreground"
                        title="移除附件"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </span>
                  ))}
                </div>
              )}

              <MentionEditor
                ref={editorRef}
                value={inputContent}
                onChange={handleInputChange}
                onKeyDown={handleKeyDown}
                onPaste={handlePaste}
                onCompositionStart={() => { isComposingRef.current = true; }}
                onCompositionEnd={() => {
                  setTimeout(() => { isComposingRef.current = false; }, 0);
                }}
                onBlur={() => setTimeout(() => setMentionOpen(false), 150)}
                disabled={!cacheKey}
                placeholder={
                  isIdle
                    ? agents.length > 0
                      ? '输入消息... (Enter 发送，@ 引用 Agent/Skill/MCP)'
                      : '输入消息... (Enter 发送，@ 引用 Skill/MCP)'
                    : '追加指示... (Enter 发送)'
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
                <SessionInputPluginHost slot="session.input-action" />
                <Button
                  onClick={handleAttachFiles}
                  disabled={!cacheKey}
                  size="icon"
                  variant="ghost"
                  className="h-8 w-8 rounded-md text-muted-foreground hover:text-foreground"
                  title="添加图片、音视频或文件"
                >
                  <Paperclip className="w-4 h-4" />
                </Button>
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
                  className={`flex items-center gap-1 hover:text-foreground transition-colors disabled:opacity-50 disabled:cursor-default disabled:hover:text-muted-foreground shrink-0 ${compact ? '' : 'truncate max-w-[300px]'}`}
                  title={sessionCwd || '点击设置对话目录'}
                >
                  <FolderOpen className="w-3 h-3 shrink-0" />
                  {!compact && <span className="truncate">{displayCwd || '设置对话目录'}</span>}
                </button>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {(displayTokens > 0 || totalTokens > 0) && (
                  <div
                    className="flex items-center gap-2 text-muted-foreground/60 tabular-nums"
                    title={
                      activeAgentLabel
                        ? `[${activeAgentLabel}] 当前 ${displayTokens.toLocaleString()} tokens / 压缩阈值 ${compressionThreshold.toLocaleString()} tokens / 总计 ${totalTokens.toLocaleString()} tokens`
                        : `当前 ${displayTokens.toLocaleString()} tokens / 压缩阈值 ${compressionThreshold.toLocaleString()} tokens / 总计 ${totalTokens.toLocaleString()} tokens`
                    }
                  >
                    {compressionThreshold > 0 && (
                      <div className="h-1.5 w-20 overflow-hidden rounded-full bg-muted">
                        <div
                          className={`h-full rounded-full transition-all ${
                            compressionProgress >= 95
                              ? 'bg-destructive'
                              : compressionProgress >= 80
                                ? 'bg-amber-500'
                                : 'bg-green-500'
                          }`}
                          style={{ width: `${compressionProgress}%` }}
                        />
                      </div>
                    )}
                    {!compact && (
                      <>
                        <span>
                          {activeAgentLabel ? `[${activeAgentLabel}] ` : ''}
                          {displayTokens.toLocaleString()}
                        </span>
                        <span>总计 {totalTokens.toLocaleString()}</span>
                      </>
                    )}
                  </div>
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
                <span>Enter 发送 · Shift+Enter 换行</span>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
