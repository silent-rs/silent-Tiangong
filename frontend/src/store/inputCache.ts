import type { RawAttachment, InputCache } from '@/api/tauri';

export type InputCacheMap = Record<string, InputCache>;

const EMPTY_INPUT_CACHE: InputCache = {
  text: '',
  attachments: [],
  is_sending: false,
  revision: 0,
};

export function emptyInputCache(): InputCache {
  return {
    text: '',
    attachments: [],
    is_sending: false,
    revision: 0,
  };
}

export function cloneInputCache(cache: InputCache): InputCache {
  return {
    ...cache,
    attachments: cache.attachments.map((attachment) => ({ ...attachment })),
  };
}

export function getInputCache(
  caches: InputCacheMap,
  key: string | null,
): InputCache {
  if (!key) return EMPTY_INPUT_CACHE;
  return caches[key] ?? EMPTY_INPUT_CACHE;
}

export function updateInputCacheText(
  caches: InputCacheMap,
  key: string,
  text: string,
): InputCacheMap {
  const current = getInputCache(caches, key);
  if (current.text === text) return caches;
  return {
    ...caches,
    [key]: {
      ...current,
      text,
      revision: current.revision + 1,
    },
  };
}

export function updateInputCacheAttachments(
  caches: InputCacheMap,
  key: string,
  attachments: RawAttachment[],
): InputCacheMap {
  const current = getInputCache(caches, key);
  if (JSON.stringify(current.attachments) === JSON.stringify(attachments)) return caches;
  return {
    ...caches,
    [key]: {
      ...current,
      attachments: attachments.map((attachment) => ({ ...attachment })),
      revision: current.revision + 1,
    },
  };
}

export function setInputCacheSending(
  caches: InputCacheMap,
  key: string,
  isSending: boolean,
): InputCacheMap {
  const current = getInputCache(caches, key);
  if (current.is_sending === isSending) return caches;
  return {
    ...caches,
    [key]: {
      ...current,
      is_sending: isSending,
    },
  };
}

export function mergeStoredInputCache(
  caches: InputCacheMap,
  key: string,
  expectedRevision: number,
  stored: InputCache,
): InputCacheMap {
  const current = caches[key];
  if (!current || current.revision !== expectedRevision) return caches;
  return {
    ...caches,
    [key]: {
      ...cloneInputCache(stored),
      // 发送态由前端发送事务维护，普通缓存同步响应不能回退它。
      is_sending: current.is_sending,
    },
  };
}

export function settleInputCacheSend(
  caches: InputCacheMap,
  key: string,
  sentRevision: number,
  succeeded: boolean,
): InputCacheMap {
  const current = caches[key];
  if (!current) return caches;
  if (succeeded && current.revision === sentRevision) {
    return {
      ...caches,
      [key]: {
        text: '',
        attachments: [],
        is_sending: false,
        revision: current.revision + 1,
      },
    };
  }
  if (!current.is_sending) return caches;
  return {
    ...caches,
    [key]: {
      ...current,
      is_sending: false,
    },
  };
}
