import { describe, expect, it } from 'vitest';
import {
  emptyInputCache,
  mergeStoredInputCache,
  settleInputCacheSend,
  updateInputCacheAttachments,
  updateInputCacheText,
  type InputCacheMap,
} from '@/store/inputCache';
import { attachmentsFromContentBlocks } from '@/utils/attachments';
import { hasMediaBlocks, textContent, type Message } from '@/api/tauri';

describe('input cache', () => {
  it('keeps text and attachments isolated between sessions', () => {
    let caches: InputCacheMap = {
      A: emptyInputCache(),
      B: emptyInputCache(),
    };
    caches = updateInputCacheText(caches, 'A', 'A input');
    caches = updateInputCacheAttachments(caches, 'B', [{
      kind: 'file',
      source: '/tmp/b.pdf',
      original_name: 'b.pdf',
      mime_type: 'application/pdf',
    }]);

    expect(caches.A.text).toBe('A input');
    expect(caches.A.attachments).toEqual([]);
    expect(caches.B.text).toBe('');
    expect(caches.B.attachments[0]?.source).toBe('/tmp/b.pdf');
  });

  it('ignores late synchronization and late send cleanup after revision changes', () => {
    let caches: InputCacheMap = { A: emptyInputCache() };
    caches = updateInputCacheText(caches, 'A', 'first');
    const firstRevision = caches.A.revision;
    caches = updateInputCacheText(caches, 'A', 'typed while sending');

    const lateResponse = {
      ...emptyInputCache(),
      text: 'first',
      revision: firstRevision,
    };
    const afterLateResponse = mergeStoredInputCache(caches, 'A', firstRevision, lateResponse);
    const afterLateCleanup = settleInputCacheSend(afterLateResponse, 'A', firstRevision, true);

    expect(afterLateResponse).toBe(caches);
    expect(afterLateCleanup.A.text).toBe('typed while sending');
    expect(afterLateCleanup.A.revision).toBeGreaterThan(firstRevision);
  });

  it('extracts ready and legacy resource blocks in their original order', () => {
    const attachments = attachmentsFromContentBlocks([
      {
        type: 'image',
        asset: {
          asset_id: 'asset-new',
          local_path: '/media/new.png',
          original_name: 'new.png',
          mime_type: 'image/png',
          size: 12,
          kind: 'image',
        },
        data: 'not-stored',
      },
      {
        type: 'media',
        kind: 'file',
        url: '/media/legacy.pdf',
        title: 'legacy.pdf',
        mime_type: 'application/pdf',
      },
      {
        type: 'asset_reference',
        asset: {
          asset_id: 'legacy-unavailable',
          local_path: '<legacy-inline-data-unavailable>',
          original_name: 'legacy.png',
          mime_type: 'image/png',
          size: 0,
          kind: 'image',
        },
      },
    ]);

    expect(attachments.map((attachment) => attachment.source)).toEqual([
      '/media/new.png',
      '/media/legacy.pdf',
    ]);
  });

  it('treats ready images and stable resource references as media', () => {
    const message = (content: Message['content']): Message => ({
      id: 'message',
      role: 'user',
      content,
      reasoning_content: '',
      created_at: '',
    });

    expect(hasMediaBlocks(message([{
      type: 'asset_reference',
      asset: {
        asset_id: 'asset',
        local_path: '/media/image.png',
        original_name: 'image.png',
        mime_type: 'image/png',
        size: 12,
        kind: 'image',
      },
    }]))).toBe(true);
    expect(hasMediaBlocks(message([{
      type: 'image',
      asset: {
        asset_id: 'runtime',
        local_path: '/media/runtime.png',
        original_name: 'runtime.png',
        mime_type: 'image/png',
        size: 12,
        kind: 'image',
      },
      data: 'ephemeral',
    }]))).toBe(true);
  });

  it('keeps model-only attachment instructions out of the visible user text', () => {
    const message: Message = {
      id: 'message',
      role: 'user',
      content: [
        { type: 'text', text: '请查看附件' },
        {
          type: 'model_instruction',
          text: '请调用附件工具并使用内部路径 /media/private.png',
        },
      ],
      reasoning_content: '',
      created_at: '',
    };

    expect(textContent(message)).toBe('请查看附件');
  });
});
