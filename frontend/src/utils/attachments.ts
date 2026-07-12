import { convertFileSrc } from '@tauri-apps/api/core';
import type { ContentBlock, RawAttachment } from '@/api/tauri';

export const MAX_ATTACHMENT_BASE64_BYTES = 50 * 1024 * 1024;

export type Attachment = RawAttachment;

export function imageMimeType(path: string): string | undefined {
  const lower = path.toLowerCase();
  if (lower.endsWith('.jpg') || lower.endsWith('.jpeg')) return 'image/jpeg';
  if (lower.endsWith('.webp')) return 'image/webp';
  if (lower.endsWith('.gif')) return 'image/gif';
  if (lower.endsWith('.png')) return 'image/png';
  return undefined;
}

export function fileMimeType(path: string): string | undefined {
  const lower = path.toLowerCase();
  const imageMime = imageMimeType(lower);
  if (imageMime) return imageMime;
  if (lower.endsWith('.pdf')) return 'application/pdf';
  if (lower.endsWith('.docx')) {
    return 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';
  }
  if (lower.endsWith('.xlsx')) {
    return 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
  }
  if (lower.endsWith('.pptx')) {
    return 'application/vnd.openxmlformats-officedocument.presentationml.presentation';
  }
  if (lower.endsWith('.txt')) return 'text/plain';
  if (lower.endsWith('.md') || lower.endsWith('.markdown')) return 'text/markdown';
  if (lower.endsWith('.json')) return 'application/json';
  if (lower.endsWith('.csv')) return 'text/csv';
  if (lower.endsWith('.mp3')) return 'audio/mpeg';
  if (lower.endsWith('.wav')) return 'audio/wav';
  if (lower.endsWith('.m4a')) return 'audio/mp4';
  if (lower.endsWith('.ogg')) return 'audio/ogg';
  if (lower.endsWith('.flac')) return 'audio/flac';
  if (lower.endsWith('.mp4')) return 'video/mp4';
  if (lower.endsWith('.mov')) return 'video/quicktime';
  if (lower.endsWith('.webm')) return 'video/webm';
  if (lower.endsWith('.mkv')) return 'video/x-matroska';
  return undefined;
}

export function attachmentKindFromMime(mimeType: string | undefined): Attachment['kind'] {
  if (mimeType?.startsWith('image/')) return 'image';
  if (mimeType?.startsWith('audio/')) return 'audio';
  if (mimeType?.startsWith('video/')) return 'video';
  return 'file';
}

export function imageExtFromMime(mimeType: string): string {
  if (mimeType === 'image/jpeg' || mimeType === 'image/jpg') return 'jpg';
  if (mimeType === 'image/webp') return 'webp';
  if (mimeType === 'image/gif') return 'gif';
  return 'png';
}

export function resolveAttachmentUrl(url: string): string {
  if (!url) return '';
  if (url.startsWith('http://') || url.startsWith('https://') || url.startsWith('asset://')) {
    return url;
  }
  if (url.startsWith('/') || /^[A-Za-z]:[\\/]/.test(url) || url.startsWith('\\\\')) {
    return convertFileSrc(url);
  }
  return url;
}

export function clipboardImagePaths(text: string): string[] {
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

export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error || new Error('读取附件失败'));
    reader.readAsDataURL(file);
  });
}

export function attachmentFromPath(path: string): Attachment {
  const mimeType = fileMimeType(path);
  return {
    kind: attachmentKindFromMime(mimeType),
    source: path,
    original_name: path.split(/[\\/]/).pop() || path,
    mime_type: mimeType,
  };
}

export function attachmentsFromContentBlocks(blocks: ContentBlock[]): Attachment[] {
  return blocks.flatMap((block): Attachment[] => {
    if (block.type === 'asset_reference' || block.type === 'image') {
      if (block.asset.local_path === '<legacy-inline-data-unavailable>') return [];
      return [{
        kind: block.asset.kind,
        source: block.asset.local_path,
        original_name: block.asset.original_name,
        mime_type: block.asset.mime_type,
      }];
    }
    if (block.type === 'media') {
      return [{
        kind: block.kind,
        source: block.url,
        original_name: block.title,
        mime_type: block.mime_type,
      }];
    }
    return [];
  });
}

export function base64SizeFromDataUrl(dataUrl: string): number {
  return dataUrl.split(',', 2)[1]?.length ?? 0;
}

export function mimeTypeFromDataUrl(dataUrl: string): string | undefined {
  const header = dataUrl.split(',', 1)[0] || '';
  const mime = header.match(/^data:([^;]+);/)?.[1];
  return mime || undefined;
}

export function assertBase64Size(dataUrl: string, title: string) {
  if (base64SizeFromDataUrl(dataUrl) > MAX_ATTACHMENT_BASE64_BYTES) {
    throw new Error(`附件"${title}"超过 50MB，已停止发送。`);
  }
}

export function estimatedBase64Size(rawBytes: number): number {
  return Math.ceil(rawBytes / 3) * 4;
}
