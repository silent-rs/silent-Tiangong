import { useSearchStore } from "@/store/useSearchStore";
import { findTextOccurrences } from "@/utils/search";
import { HighlightText } from "../HighlightText";
import { Textarea } from "../ui/textarea";
import { X, Paperclip } from "lucide-react";
import { resolveAttachmentUrl, type Attachment } from "@/utils/attachments";
import { textContent } from "@/api/tauri";
import { formatMessageTime } from "./utils";
import type { MessageGroup } from "./types";
import { VoiceBubble } from "./VoiceBubble";
import { UserMessageActions } from "./UserMessageActions";
import { ContentMedia } from "./ContentMedia";

export function UserMessageGroup({ group, runStatus, nonEditableIds, voiceMessages, editingMessageId, editingContent, editingAttachments, editingTextareaRef, hasMultimodal, onStartEdit, onConfirmEdit, onCancelEdit, onSetEditingContent, onSetEditingAttachments, onAttachFiles, onEditPaste }: {
  group: MessageGroup;
  runStatus: string;
  nonEditableIds: Set<string>;
  voiceMessages: Record<string, { audioPath: string; duration?: number; showText: boolean }>;
  editingMessageId: string | null;
  editingContent: string;
  editingAttachments: Attachment[];
  editingTextareaRef: React.RefObject<HTMLTextAreaElement>;
  hasMultimodal: boolean;
  onStartEdit: (messageId: string, text: string) => void;
  onConfirmEdit: () => void;
  onCancelEdit: () => void;
  onSetEditingContent: (v: string) => void;
  onSetEditingAttachments: React.Dispatch<React.SetStateAction<Attachment[]>>;
  onAttachFiles: () => void;
  onEditPaste: (e: React.ClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const message = group.messages[0];
  const voiceInfo = voiceMessages[message.id];
  const isEditing = editingMessageId === message.id;
  const searchQuery = useSearchStore((s) => s.searchQuery);
  const currentMessageId = useSearchStore((s) => s.currentMessageId);
  const currentMatchStart = useSearchStore((s) => s.currentMatchStart);
  const caseSensitive = useSearchStore((s) => s.caseSensitive);

  const renderUserText = (text: string) => {
    if (!searchQuery) return text;
    const occurrences = findTextOccurrences(text, searchQuery, caseSensitive);
    if (occurrences.length === 0) return text;
    const isCurrent = message.id === currentMessageId;
    return <HighlightText text={text} matches={occurrences} currentMatchStart={isCurrent ? currentMatchStart : null} />;
  };

  return (
    <div className="mt-3 first:mt-0">
      {isEditing ? (
        <div className="w-full">
          {editingAttachments.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-1.5">
              {editingAttachments.map((item) => (
                <span key={item.title + item.url.slice(0, 40)} className="inline-flex h-9 max-w-[260px] items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs" title={item.title}>
                  {item.kind === "image" ? <img src={resolveAttachmentUrl(item.url)} alt={item.title} className="h-6 w-6 shrink-0 rounded object-cover" /> : <Paperclip className="h-3 w-3 shrink-0" />}
                  <span className="truncate">{item.title}</span>
                  <button type="button" onClick={() => onSetEditingAttachments((prev) => prev.filter((a) => a.url !== item.url))} className="ml-1 text-muted-foreground hover:text-foreground" title="移除附件">
                    <X className="h-3 w-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
          <Textarea
            ref={editingTextareaRef}
            value={editingContent}
            onChange={(e) => {
              onSetEditingContent(e.target.value);
              const textarea = editingTextareaRef.current;
              if (textarea) { textarea.style.height = "60px"; textarea.style.height = Math.min(textarea.scrollHeight, 200) + "px"; }
            }}
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
              {hasMultimodal && (
                <button onClick={onAttachFiles} className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors" title="添加附件">
                  <Paperclip className="w-3 h-3" />
                </button>
              )}
              <button onClick={onCancelEdit} className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors">
                <X className="h-3 w-3" /> 取消
              </button>
              <button onClick={onConfirmEdit} className="px-2.5 py-1 text-xs bg-green-600 hover:bg-green-700 text-white rounded transition-colors">发送</button>
            </div>
          </div>
        </div>
      ) : (
        <div className="flex justify-end" title={formatMessageTime(message.created_at)}>
          <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-foreground">
            {voiceInfo ? (
              <VoiceBubble messageId={message.id} audioPath={voiceInfo.audioPath} duration={voiceInfo.duration} showText={voiceInfo.showText} content={textContent(message)} />
            ) : (
              <div>
                <ContentMedia message={message} />
                {textContent(message) && <p className="whitespace-pre-wrap break-words text-sm">{renderUserText(textContent(message))}</p>}
              </div>
            )}
          </div>
        </div>
      )}
      {textContent(message) && !isEditing && (
        <div className="flex justify-end">
          <UserMessageActions text={textContent(message)} messageId={message.id} runStatus={runStatus} canEdit={!nonEditableIds.has(message.id)} onStartEdit={onStartEdit} />
        </div>
      )}
    </div>
  );
}
