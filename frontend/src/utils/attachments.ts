import { convertFileSrc } from '@tauri-apps/api/core';
import { api, type MediaAsset } from '@/api/tauri';

export const MAX_ATTACHMENT_BASE64_BYTES = 50 * 1024 * 1024;

export type Attachment = {
  kind: 'image' | 'file';
  url: string;
  title: string;
  mime_type?: string;
};

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
  return undefined;
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
  if (url.startsWith('/')) {
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
  const lower = path.toLowerCase();
  const isImage = /\.(png|jpe?g|webp|gif)$/.test(lower);
  return {
    kind: isImage ? 'image' : 'file',
    url: path,
    title: path.split('/').pop() || path,
    mime_type: fileMimeType(path),
  };
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

/** 判断 URL 是否为已归档到本地的媒体路径（~/.tiangong/media/...）。
 *  统一正反斜杠后判断（Windows 路径兼容）。 */
export function isArchivedMediaPath(url: string): boolean {
  return url.replace(/\\/g, '/').includes('/.tiangong/media/');
}

export async function attachmentToBase64Media(item: Attachment): Promise<MediaAsset> {
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

  // 已归档到本地的路径直接复用，不重新读取为 base64——避免编辑时反复产生重复附件。
  if (isArchivedMediaPath(item.url)) {
    return {
      kind: item.kind,
      url: item.url,
      title: item.title,
      mime_type: item.mime_type,
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
}

export async function attachmentsToBase64Media(items: Attachment[]): Promise<MediaAsset[]> {
  const media: MediaAsset[] = [];
  for (const item of items) {
    media.push(await attachmentToBase64Media(item));
  }
  return media;
}
