import { useState } from "react";
import { useSearchStore } from "@/store/useSearchStore";
import { findTextOccurrences } from "@/utils/search";
import { HighlightText } from "../HighlightText";
import { MentionChip } from "../MentionChip";
import { MentionEditor, type MentionEditorHandle } from "../MentionEditor";
import { Clock3, Webhook as WebhookIcon, X, Paperclip } from "lucide-react";
import { resolveAttachmentUrl, type Attachment } from "@/utils/attachments";
import { parseScheduledTaskMessage } from "@/utils/scheduledTaskMessage";
import { parseWebhookMessage } from "@/utils/webhookMessage";
import { textContent } from "@/api/tauri";
import { hasMention, parseBlocks } from "@/utils/mentionBlocks";
import { formatMessageTime } from "./utils";
import type { MessageGroup } from "./types";
import { VoiceBubble } from "./VoiceBubble";
import { UserMessageActions } from "./UserMessageActions";
import { ContentMedia } from "./ContentMedia";
import { CollapsibleUserText } from "./CollapsibleUserText";


/// Subagent 任务指派卡片：分层展示核心任务与元信息。
/// 正文结构：任务/消息内容 →【任务工作区】→【长期指令】→【长期记忆】
/// →【成长约定】→【工作区状态】→（运行标记）。
function SubagentTaskCard({ body, source, kind, time, onCopy, renderText }: {
  body: string; source: string; kind: string; time: string;
  onCopy: () => void; renderText: (text: string) => React.ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const [metaOpen, setMetaOpen] = useState(false);

  const metaMarkers = ['\u3010任务工作区\u3011', '\u3010长期指令\u3011', '\u3010长期记忆\u3011', '\u3010成长约定\u3011', '\u3010工作区状态\u3011'];
  let coreContent = body;
  const metaSections: { title: string; content: string }[] = [];
  for (const marker of metaMarkers) {
    const idx = coreContent.indexOf(marker);
    if (idx >= 0) {
      let end = coreContent.length;
      for (const next of metaMarkers) {
        const nextIdx = coreContent.indexOf(next, idx + marker.length);
        if (nextIdx >= 0 && nextIdx < end) end = nextIdx;
      }
      const tagIdx = coreContent.indexOf('\uff08运行标记', idx);
      if (tagIdx >= 0 && tagIdx < end) end = tagIdx;
      const content = coreContent.slice(idx + marker.length, end).trim();
      if (content) metaSections.push({ title: marker.slice(1, -1), content });
      coreContent = coreContent.slice(0, idx).trimEnd();
    }
  }
  coreContent = coreContent.replace(/\n*\uff08运行标记[^\uff09]*\uff09\s*$/, '').trim();

  const TRUNCATE_LEN = 300;
  const isLong = coreContent.length > TRUNCATE_LEN;
  const displayContent = expanded || !isLong ? coreContent : coreContent.slice(0, TRUNCATE_LEN) + '…';

  const isTask = kind === '任务';

  return (
    <div className="w-full max-w-[92%] sm:max-w-[80%]">
      <div className="rounded-xl border border-border/60 bg-card shadow-sm overflow-hidden">
        <div className="flex items-center gap-2.5 px-4 py-2 border-b border-border/30 bg-muted/[0.1]">
          <div className="flex items-center justify-center w-7 h-7 rounded-lg bg-primary/10 border border-primary/15 shrink-0">
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="w-3.5 h-3.5 text-primary">
              {isTask ? <><path d="M9 11l3 3L22 4"/><path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11"/></> : <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>}
            </svg>
          </div>
          <div className="min-w-0 flex-1">
            <span className="text-sm font-semibold text-foreground">{isTask ? '任务指派' : '协作消息'}</span>
            <span className="ml-2 text-[11px] text-muted-foreground">来自 {source.trim()}</span>
          </div>
          <span className="text-[10px] text-muted-foreground/50 shrink-0">{time}</span>
          <button type="button" aria-label="复制" className="inline-flex items-center justify-center w-6 h-6 rounded-md text-muted-foreground/40 hover:text-foreground hover:bg-foreground/5 transition-colors shrink-0" onClick={onCopy}>
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="w-3.5 h-3.5"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
          </button>
        </div>

        <div className="px-4 py-3">
          <div className="text-sm leading-relaxed whitespace-pre-wrap break-words text-card-foreground">
            {renderText(displayContent)}
          </div>
          {isLong && (
            <button type="button" className="mt-2 text-xs text-primary hover:text-primary/70 transition-colors" onClick={() => setExpanded(!expanded)}>
              {expanded ? '收起' : `展开全部（${coreContent.length} 字）`}
            </button>
          )}
        </div>

        {metaSections.length > 0 && (
          <div className="border-t border-border/30">
            <button type="button" className="flex items-center gap-1.5 w-full px-4 py-1.5 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/20 transition-colors" onClick={() => setMetaOpen(!metaOpen)}>
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={`w-3 h-3 transition-transform ${metaOpen ? 'rotate-90' : ''}`}><path d="m9 18 6-6-6-6"/></svg>
              {metaOpen ? '收起附加信息' : `附加信息（${metaSections.length} 项：${metaSections.map(s => s.title).join('、')}）`}
            </button>
            {metaOpen && (
              <div className="px-4 pb-3 space-y-2">
                {metaSections.map((section) => (
                  <div key={section.title} className="rounded-lg bg-muted/[0.12] border border-border/20 overflow-hidden">
                    <p className="px-3 py-1 text-[10px] font-medium text-muted-foreground border-b border-border/15">{section.title}</p>
                    <div className="px-3 py-1.5 text-xs leading-relaxed whitespace-pre-wrap break-words text-muted-foreground max-h-32 overflow-y-auto">
                      {renderText(section.content)}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

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
  const webhook = parseWebhookMessage(messageText);
  // Subagent 回报：Hook 消息以 [Subagent·成员名] 开头，渲染为卡片
  // 而非普通用户消息气泡。
  const subagentMatch = messageText.match(/^\[Subagent·([^\]]+)\]\s*(.*)$/s);
  // Subagent 任务投递：专属会话中收到的任务/消息以【Subagent 消息/任务】开头。
  const subagentTaskMatch = messageText.match(/^【Subagent (消息|任务)】来自(.+?)：\n?([\s\S]*)$/);
  const voiceInfo = voiceMessages[message.id];
  const isEditing = editingMessageId === message.id && !scheduledTask && !webhook;
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
          {subagentTaskMatch ? (() => {
            const [, kind, source, body] = subagentTaskMatch;
            return (
              <SubagentTaskCard
                body={body}
                source={source}
                kind={kind}
                time={formatMessageTime(message.created_at)}
                onCopy={() => navigator.clipboard.writeText(messageText).catch(() => {})}
                renderText={renderUserText}
              />
            );
          })() : subagentMatch ? (() => {
            const [, agentName, body] = subagentMatch;
            const statusMatch = body.match(/^(任务完成|执行失败|运行阻塞|等待审批)：?\s*/);
            const status = statusMatch ? statusMatch[1] : "消息";
            const content = statusMatch ? body.slice(statusMatch[0].length) : body;
            const statusIcon: Record<string, string> = { "任务完成": "✓", "执行失败": "✗", "运行阻塞": "⏳", "等待审批": "?" };
            const accentColor: Record<string, string> = {
              "任务完成": "border-l-emerald-500",
              "执行失败": "border-l-red-500",
              "运行阻塞": "border-l-amber-500",
              "等待审批": "border-l-blue-500",
              "消息": "border-l-border",
            };
            const iconBg: Record<string, string> = {
              "任务完成": "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border-emerald-500/20",
              "执行失败": "bg-red-500/10 text-red-600 dark:text-red-400 border-red-500/20",
              "运行阻塞": "bg-amber-500/10 text-amber-600 dark:text-amber-400 border-amber-500/20",
              "等待审批": "bg-blue-500/10 text-blue-600 dark:text-blue-400 border-blue-500/20",
              "消息": "bg-muted text-muted-foreground border-border",
            };
            const pillColor: Record<string, string> = {
              "任务完成": "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300 border-emerald-500/15",
              "执行失败": "bg-red-500/10 text-red-700 dark:text-red-300 border-red-500/15",
              "运行阻塞": "bg-amber-500/10 text-amber-700 dark:text-amber-300 border-amber-500/15",
              "等待审批": "bg-blue-500/10 text-blue-700 dark:text-blue-300 border-blue-500/15",
              "消息": "bg-muted/50 text-muted-foreground border-border",
            };
            const ac = accentColor[status] || accentColor["消息"];
            const ib = iconBg[status] || iconBg["消息"];
            const pc = pillColor[status] || pillColor["消息"];
            return (
              <div className="w-full max-w-[92%] sm:max-w-[80%]">
                <div className={`rounded-xl border border-border/60 border-l-[3px] ${ac} bg-card shadow-sm overflow-hidden`}>
                  <div className="flex items-center gap-2.5 px-4 py-2.5 border-b border-border/30 bg-muted/[0.15]">
                    <div className={`flex items-center justify-center w-7 h-7 rounded-full border shrink-0 ${ib}`}>
                      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" className="w-3.5 h-3.5">
                        {status === "任务完成" && <path d="M20 6 9 17l-5-5"/>}
                        {status === "执行失败" && <><path d="M18 6 6 18"/><path d="m6 6 12 12"/></>}
                        {status === "运行阻塞" && <><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></>}
                        {status === "等待审批" && <><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></>}
                        {status === "消息" && <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z"/>}
                      </svg>
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="text-sm font-semibold text-foreground">{agentName.trim()}</span>
                        <span className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium border ${pc}`}>
                          {statusIcon[status] || "•"} {status}
                        </span>
                      </div>
                    </div>
                    <span className="text-[10px] text-muted-foreground/50 shrink-0">{formatMessageTime(message.created_at)}</span>
                    <button type="button" aria-label="复制" className="inline-flex items-center justify-center w-6 h-6 rounded-md text-muted-foreground/40 hover:text-foreground hover:bg-foreground/5 transition-colors shrink-0" onClick={() => navigator.clipboard.writeText(messageText).catch(() => {})}>
                      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="w-3.5 h-3.5"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
                    </button>
                  </div>
                  <div className="px-4 py-3 text-sm leading-relaxed whitespace-pre-wrap break-words text-card-foreground">
                    {renderUserText(content.trim())}
                  </div>
                </div>
              </div>
            );
          })() : scheduledTask || webhook ? (
            (() => {
              const data = scheduledTask ?? webhook!;
              return (
                <div className="w-full max-w-[92%] overflow-hidden rounded-lg border border-border/70 bg-card text-foreground shadow-sm sm:max-w-[85%]">
                  <div className="flex items-start gap-2.5 bg-muted/35 px-3 py-2.5">
                    {webhook
                      ? <WebhookIcon className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                      : <Clock3 className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" aria-hidden="true" />}
                    <div className="min-w-0 flex-1">
                      <p className="text-xs font-medium text-muted-foreground">{webhook ? 'Webhook 触发' : '定时任务'}</p>
                      <p className="mt-0.5 break-words text-sm font-medium">
                        {data.name
                          ? renderUserText(data.name, data.offsets.name)
                          : webhook ? '未命名触发' : '未命名任务'}
                      </p>
                      {data.description && (
                        <p className="mt-1 whitespace-pre-wrap break-words text-xs text-muted-foreground">
                          {renderUserText(data.description, data.offsets.description)}
                        </p>
                      )}
                    </div>
                  </div>
                  <div className="border-t border-border/60 px-3 py-2.5">
                    <p className="mb-1 text-[11px] font-medium text-muted-foreground">执行内容</p>
                    <ContentMedia message={message} />
                    {data.payload ? (
                      <p className="whitespace-pre-wrap break-words text-sm leading-6">
                        {renderUserText(data.payload, data.offsets.payload)}
                      </p>
                    ) : (
                      <p className="text-sm text-muted-foreground">无执行内容</p>
                    )}
                  </div>
                </div>
              );
            })()
          ) : (
            <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-foreground">
              {voiceInfo ? (
                <VoiceBubble messageId={message.id} audioPath={voiceInfo.audioPath} duration={voiceInfo.duration} showText={voiceInfo.showText} content={messageText} />
              ) : (
                <div>
                  <ContentMedia message={message} />
                  {messageText && (
                    <CollapsibleUserText messageId={message.id}>
                      {renderUserText(messageText)}
                    </CollapsibleUserText>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      )}
      {messageText && !isEditing && !subagentMatch && !subagentTaskMatch && (
        <div className="flex justify-end">
          <UserMessageActions text={messageText} messageId={message.id} runStatus={runStatus} canEdit={!nonEditableIds.has(message.id)} showEdit={!scheduledTask && !webhook} onStartEdit={onStartEdit} />
        </div>
      )}
    </div>
  );
}
