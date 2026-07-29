import { useSearchStore } from "@/store/useSearchStore";
import { findTextOccurrences } from "@/utils/search";
import { HighlightText } from "../HighlightText";
import { MentionChip } from "../MentionChip";
import { MentionEditor, type MentionEditorHandle } from "../MentionEditor";
import { Clock3, X, Paperclip } from "lucide-react";
import { resolveAttachmentUrl, type Attachment } from "@/utils/attachments";
import { parseScheduledTaskMessage } from "@/utils/scheduledTaskMessage";
import { textContent } from "@/api/tauri";
import { hasMention, parseBlocks } from "@/utils/mentionBlocks";
import { formatMessageTime } from "./utils";
import type { MessageGroup } from "./types";
import { VoiceBubble } from "./VoiceBubble";
import { UserMessageActions } from "./UserMessageActions";
import { ContentMedia } from "./ContentMedia";

export function UserMessageGroup({ group, runStatus, nonEditableIds, voiceMessages, editingMessageId, editingContent, editingAttachments, editingTextareaRef, onStartEdit, onConfirmEdit, onCancelEdit, onSetEditingContent, onSetEditingAttachments, onAttachFiles, onEditPaste }: {
  group: MessageGroup;
  runStatus: string;
  nonEditableIds: Set<string>;
  voiceMessages: Record<string, { audioPath: string; duration?: number; showText: boolean }>;
  editingMessageId: string | null;
  editingContent: string;
  editingAttachments: Attachment[];
  editingTextareaRef: React.RefObject<MentionEditorHandle>;
  onStartEdit: (messageId: string, text: string) => void;
  onConfirmEdit: () => void;
  onCancelEdit: () => void;
  onSetEditingContent: (v: string) => void;
  onSetEditingAttachments: React.Dispatch<React.SetStateAction<Attachment[]>>;
  onAttachFiles: () => void;
  onEditPaste: (e: React.ClipboardEvent<HTMLDivElement>) => void;
}) {
  const message = group.messages[0];
  const messageText = textContent(message);
  const scheduledTask = parseScheduledTaskMessage(messageText);
  const voiceInfo = voiceMessages[message.id];
  const isEditing = editingMessageId === message.id && !scheduledTask;
  const searchQuery = useSearchStore((s) => s.searchQuery);
  const currentMessageId = useSearchStore((s) => s.currentMessageId);
  const currentMatchStart = useSearchStore((s) => s.currentMatchStart);
  const caseSensitive = useSearchStore((s) => s.caseSensitive);

  const renderUserText = (text: string, sourceOffset = 0) => {
    const isCurrent = message.id === currentMessageId;
    const localCurrentMatch = isCurrent
      && currentMatchStart !== null
      && currentMatchStart >= sourceOffset
      && currentMatchStart < sourceOffset + text.length
      ? currentMatchStart - sourceOffset
      : null;

    // 无提及：走原有纯文本 / 高亮路径，行为完全不变
    if (!hasMention(text)) {
      if (!searchQuery) return text;
      const occurrences = findTextOccurrences(text, searchQuery, caseSensitive);
      if (occurrences.length === 0) return text;
      return <HighlightText text={text} matches={occurrences} currentMatchStart={localCurrentMatch} />;
    }

    // 有提及：按块分段渲染。搜索高亮的 offset 空间仍是原始字符串，
    // 因此对每个 text 段切分其在全串中的匹配，保留既有高亮语义。
    const allOccurrences = searchQuery
      ? findTextOccurrences(text, searchQuery, caseSensitive)
      : [];
    const nodes: React.ReactNode[] = [];
    let segStart = 0;
    let key = 0;
    for (const block of parseBlocks(text)) {
      if (block.type === 'text') {
        const segEnd = segStart + block.value.length;
        const segMatches = allOccurrences
          .filter((m) => m.start >= segStart && m.end <= segEnd)
          .map((m) => ({ start: m.start - segStart, end: m.end - segStart }));
        if (segMatches.length === 0) {
          nodes.push(<span key={key++}>{block.value}</span>);
        } else {
          // 当前搜索位置已换算到本分区，落在本段内时再按段起点换算。
          const rebasedCurrent = localCurrentMatch != null
            && localCurrentMatch >= segStart && localCurrentMatch < segEnd
              ? localCurrentMatch - segStart
              : null;
          nodes.push(
            <HighlightText
              key={key++}
              text={block.value}
              matches={segMatches}
              currentMatchStart={rebasedCurrent}
            />,
          );
        }
        segStart = segEnd;
      } else {
        nodes.push(
          <MentionChip
            key={key++}
            kind={block.kind}
            label={block.label}
            token={block.token}
          />,
        );
        segStart += block.token.length;
      }
    }
    return <>{nodes}</>;
  };

  return (
    <div className="mt-3 first:mt-0">
      {isEditing ? (
        <div className="w-full">
          {editingAttachments.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-1.5">
              {editingAttachments.map((item) => (
                <span key={(item.original_name ?? '') + item.source.slice(0, 40)} className="inline-flex h-9 max-w-[260px] items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs" title={item.original_name ?? item.source}>
                  {item.kind === "image" ? <img src={resolveAttachmentUrl(item.source)} alt={item.original_name ?? '附件'} className="h-6 w-6 shrink-0 rounded object-cover" /> : <Paperclip className="h-3 w-3 shrink-0" />}
                  <span className="truncate">{item.original_name ?? item.source}</span>
                  <button type="button" onClick={() => onSetEditingAttachments((prev) => prev.filter((a) => a.source !== item.source))} className="ml-1 text-muted-foreground hover:text-foreground" title="移除附件">
                    <X className="h-3 w-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
          <MentionEditor
            ref={editingTextareaRef}
            value={editingContent}
            onChange={onSetEditingContent}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && e.keyCode !== 229) { e.preventDefault(); onConfirmEdit(); }
              if (e.key === "Escape") { onCancelEdit(); }
            }}
            onPaste={onEditPaste}
            className="min-h-[60px] max-h-[200px] resize-none text-sm w-full"
            autoFocus
          />
          <div className="flex justify-between items-center mt-1">
            <span className="text-[10px] text-muted-foreground">Enter 发送 · Shift+Enter 换行 · Esc 取消</span>
            <div className="flex gap-1.5">
              <button onClick={onAttachFiles} className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors" title="添加附件">
                <Paperclip className="w-3 h-3" />
              </button>
              <button onClick={onCancelEdit} className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors">
                <X className="h-3 w-3" /> 取消
              </button>
              <button onClick={onConfirmEdit} className="px-2.5 py-1 text-xs bg-green-600 hover:bg-green-700 text-white rounded transition-colors">发送</button>
            </div>
          </div>
        </div>
      ) : (
        <div className="flex justify-end" title={formatMessageTime(message.created_at)}>
          {scheduledTask ? (
            <div className="w-full max-w-[92%] overflow-hidden rounded-lg border border-border/70 bg-card text-foreground shadow-sm sm:max-w-[85%]">
              <div className="flex items-start gap-2.5 bg-muted/35 px-3 py-2.5">
                <Clock3 className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                <div className="min-w-0 flex-1">
                  <p className="text-xs font-medium text-muted-foreground">定时任务</p>
                  <p className="mt-0.5 break-words text-sm font-medium">
                    {scheduledTask.name
                      ? renderUserText(scheduledTask.name, scheduledTask.offsets.name)
                      : '未命名任务'}
                  </p>
                  {scheduledTask.description && (
                    <p className="mt-1 whitespace-pre-wrap break-words text-xs text-muted-foreground">
                      {renderUserText(scheduledTask.description, scheduledTask.offsets.description)}
                    </p>
                  )}
                </div>
              </div>
              <div className="border-t border-border/60 px-3 py-2.5">
                <p className="mb-1 text-[11px] font-medium text-muted-foreground">执行内容</p>
                <ContentMedia message={message} />
                {scheduledTask.payload ? (
                  <p className="whitespace-pre-wrap break-words text-sm leading-6">
                    {renderUserText(scheduledTask.payload, scheduledTask.offsets.payload)}
                  </p>
                ) : (
                  <p className="text-sm text-muted-foreground">无执行内容</p>
                )}
              </div>
            </div>
          ) : (
            <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-foreground">
              {voiceInfo ? (
                <VoiceBubble messageId={message.id} audioPath={voiceInfo.audioPath} duration={voiceInfo.duration} showText={voiceInfo.showText} content={messageText} />
              ) : (
                <div>
                  <ContentMedia message={message} />
                  {messageText && <p className="whitespace-pre-wrap break-words text-sm">{renderUserText(messageText)}</p>}
                </div>
              )}
            </div>
          )}
        </div>
      )}
      {messageText && !isEditing && (
        <div className="flex justify-end">
          <UserMessageActions text={messageText} messageId={message.id} runStatus={runStatus} canEdit={!nonEditableIds.has(message.id)} showEdit={!scheduledTask} onStartEdit={onStartEdit} />
        </div>
      )}
    </div>
  );
}
