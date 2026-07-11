import type { RawAttachment, SessionInputDraft } from '@/api/tauri';

export type SessionDraftMap = Record<string, SessionInputDraft>;

const EMPTY_DRAFT: SessionInputDraft = {
  text: '',
  attachments: [],
  is_sending: false,
  revision: 0,
};

export function emptySessionInputDraft(): SessionInputDraft {
  return {
    text: '',
    attachments: [],
    is_sending: false,
    revision: 0,
  };
}

export function cloneSessionInputDraft(draft: SessionInputDraft): SessionInputDraft {
  return {
    ...draft,
    attachments: draft.attachments.map((attachment) => ({ ...attachment })),
  };
}

export function getSessionInputDraft(
  drafts: SessionDraftMap,
  key: string | null,
): SessionInputDraft {
  if (!key) return EMPTY_DRAFT;
  return drafts[key] ?? EMPTY_DRAFT;
}

export function updateDraftText(
  drafts: SessionDraftMap,
  key: string,
  text: string,
): SessionDraftMap {
  const current = getSessionInputDraft(drafts, key);
  if (current.text === text) return drafts;
  return {
    ...drafts,
    [key]: {
      ...current,
      text,
      revision: current.revision + 1,
    },
  };
}

export function updateDraftAttachments(
  drafts: SessionDraftMap,
  key: string,
  attachments: RawAttachment[],
): SessionDraftMap {
  const current = getSessionInputDraft(drafts, key);
  if (JSON.stringify(current.attachments) === JSON.stringify(attachments)) return drafts;
  return {
    ...drafts,
    [key]: {
      ...current,
      attachments: attachments.map((attachment) => ({ ...attachment })),
      revision: current.revision + 1,
    },
  };
}

export function setDraftSending(
  drafts: SessionDraftMap,
  key: string,
  isSending: boolean,
): SessionDraftMap {
  const current = getSessionInputDraft(drafts, key);
  if (current.is_sending === isSending) return drafts;
  return {
    ...drafts,
    [key]: {
      ...current,
      is_sending: isSending,
    },
  };
}

export function mergePersistedDraft(
  drafts: SessionDraftMap,
  key: string,
  expectedRevision: number,
  persisted: SessionInputDraft,
): SessionDraftMap {
  const current = drafts[key];
  if (!current || current.revision !== expectedRevision) return drafts;
  return {
    ...drafts,
    [key]: {
      ...cloneSessionInputDraft(persisted),
      // 发送态由前端发送事务维护，普通草稿持久化响应不能回退它。
      is_sending: current.is_sending,
    },
  };
}

export function migrateDraftKey(
  drafts: SessionDraftMap,
  fromKey: string,
  toKey: string,
): SessionDraftMap {
  if (fromKey === toKey) return drafts;
  const source = drafts[fromKey] ?? emptySessionInputDraft();
  const next = { ...drafts, [toKey]: cloneSessionInputDraft(source) };
  delete next[fromKey];
  return next;
}

export function settleDraftSend(
  drafts: SessionDraftMap,
  key: string,
  sentRevision: number,
  succeeded: boolean,
): SessionDraftMap {
  const current = drafts[key];
  if (!current) return drafts;
  if (succeeded && current.revision === sentRevision) {
    return {
      ...drafts,
      [key]: {
        text: '',
        attachments: [],
        is_sending: false,
        revision: current.revision + 1,
      },
    };
  }
  if (!current.is_sending) return drafts;
  return {
    ...drafts,
    [key]: {
      ...current,
      is_sending: false,
    },
  };
}
