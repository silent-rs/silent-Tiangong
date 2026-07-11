import { describe, expect, it } from 'vitest';
import {
  emptySessionInputDraft,
  mergePersistedDraft,
  migrateDraftKey,
  settleDraftSend,
  updateDraftAttachments,
  updateDraftText,
  type SessionDraftMap,
} from '@/store/sessionDrafts';
import { attachmentsFromContentBlocks } from '@/utils/attachments';
import { hasMediaBlocks, type Message } from '@/api/tauri';

describe('session drafts', () => {
  it('keeps text and attachments isolated between sessions', () => {
    let drafts: SessionDraftMap = {
      A: emptySessionInputDraft(),
      B: emptySessionInputDraft(),
    };
    drafts = updateDraftText(drafts, 'A', 'A draft');
    drafts = updateDraftAttachments(drafts, 'B', [{
      kind: 'file',
      source: '/tmp/b.pdf',
      original_name: 'b.pdf',
      mime_type: 'application/pdf',
    }]);

    expect(drafts.A.text).toBe('A draft');
    expect(drafts.A.attachments).toEqual([]);
    expect(drafts.B.text).toBe('');
    expect(drafts.B.attachments[0]?.source).toBe('/tmp/b.pdf');
  });

  it('ignores late persistence and late send cleanup after revision changes', () => {
    let drafts: SessionDraftMap = { A: emptySessionInputDraft() };
    drafts = updateDraftText(drafts, 'A', 'first');
    const firstRevision = drafts.A.revision;
    drafts = updateDraftText(drafts, 'A', 'typed while sending');

    const lateResponse = {
      ...emptySessionInputDraft(),
      text: 'first',
      revision: firstRevision,
    };
    const afterLateResponse = mergePersistedDraft(drafts, 'A', firstRevision, lateResponse);
    const afterLateCleanup = settleDraftSend(afterLateResponse, 'A', firstRevision, true);

    expect(afterLateResponse).toBe(drafts);
    expect(afterLateCleanup.A.text).toBe('typed while sending');
    expect(afterLateCleanup.A.revision).toBeGreaterThan(firstRevision);
  });

  it('migrates a temporary draft key without losing state', () => {
    const temporary = updateDraftAttachments(
      updateDraftText({ temp: emptySessionInputDraft() }, 'temp', 'new session'),
      'temp',
      [{ kind: 'image', source: 'data:image/png;base64,AA==', original_name: 'paste.png' }],
    );

    const migrated = migrateDraftKey(temporary, 'temp', 'real-session');

    expect(migrated.temp).toBeUndefined();
    expect(migrated['real-session']).toEqual(temporary.temp);
    expect(migrated['real-session']).not.toBe(temporary.temp);
  });

  it('extracts new and legacy attachment blocks in their original order', () => {
    const attachments = attachmentsFromContentBlocks([
      {
        type: 'attachment',
        attachment: {
          asset_id: 'asset-new',
          local_path: '/media/new.png',
          original_name: 'new.png',
          mime_type: 'image/png',
          size: 12,
          kind: 'image',
          handling_mode: 'inline_image',
          capability_available: true,
        },
      },
      {
        type: 'runtime_inline_image',
        asset_id: 'runtime-only',
        mime_type: 'image/png',
        data: 'not-persisted',
      },
      {
        type: 'media',
        kind: 'file',
        url: '/media/legacy.pdf',
        title: 'legacy.pdf',
        mime_type: 'application/pdf',
      },
    ]);

    expect(attachments.map((attachment) => attachment.source)).toEqual([
      '/media/new.png',
      '/media/legacy.pdf',
    ]);
  });

  it('treats persistent attachments as media but ignores runtime-only images', () => {
    const message = (content: Message['content']): Message => ({
      id: 'message',
      role: 'user',
      content,
      reasoning_content: '',
      created_at: '',
    });

    expect(hasMediaBlocks(message([{
      type: 'attachment',
      attachment: {
        asset_id: 'asset',
        local_path: '/media/image.png',
        original_name: 'image.png',
        mime_type: 'image/png',
        size: 12,
        kind: 'image',
        handling_mode: 'inline_image',
        capability_available: true,
      },
    }]))).toBe(true);
    expect(hasMediaBlocks(message([{
      type: 'runtime_inline_image',
      asset_id: 'runtime',
      mime_type: 'image/png',
      data: 'ephemeral',
    }]))).toBe(false);
  });
});
