import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://mocked/${path}`,
}));

import {
  clipboardImagePaths,
  fileUrlToLocalPath,
  resolveAttachmentUrl,
} from '@/utils/attachments';

const converted = (path: string) => `asset://mocked/${path}`;

describe('fileUrlToLocalPath', () => {
  it('还原 Unix 本地路径', () => {
    expect(fileUrlToLocalPath('file:///Users/test/image.png')).toBe('/Users/test/image.png');
  });

  it('还原 Windows 盘符路径（剥离盘符前导斜杠）', () => {
    expect(fileUrlToLocalPath('file:///C:/Users/test/image.png')).toBe('C:/Users/test/image.png');
  });

  it('还原 UNC 路径（主机名按服务器处理）', () => {
    expect(fileUrlToLocalPath('file://server/share/image.png')).toBe(
      '\\\\server/share/image.png',
    );
  });

  it('localhost 主机视为本机路径', () => {
    expect(fileUrlToLocalPath('file://localhost/Users/test/image.png')).toBe(
      '/Users/test/image.png',
    );
    expect(fileUrlToLocalPath('file://localhost/C:/Users/test/image.png')).toBe(
      'C:/Users/test/image.png',
    );
  });

  it('文件名中的 # 与 ? 不丢失（不被当作 fragment/query 切断）', () => {
    expect(fileUrlToLocalPath('file:///tmp/report#1.png')).toBe('/tmp/report#1.png');
    expect(fileUrlToLocalPath('file:///tmp/report?draft.png')).toBe('/tmp/report?draft.png');
  });

  it('scheme 大小写不敏感', () => {
    expect(fileUrlToLocalPath('FILE:///Users/test/image.png')).toBe('/Users/test/image.png');
    expect(fileUrlToLocalPath('File:///Users/test/image.png')).toBe('/Users/test/image.png');
  });

  it('解码百分号编码，非法序列保留原样', () => {
    expect(fileUrlToLocalPath('file:///tmp/report%20draft.png')).toBe('/tmp/report draft.png');
    expect(fileUrlToLocalPath('file:///C:/docs/%E6%8A%A5%E8%A1%A8.pdf')).toBe('C:/docs/报表.pdf');
    expect(fileUrlToLocalPath('file:///tmp/100%.png')).toBe('/tmp/100%.png');
  });

  it('file://C:/x 误写的主机位盘符', () => {
    expect(fileUrlToLocalPath('file://C:/Users/test/image.png')).toBe('C:/Users/test/image.png');
  });

  it('非 file 协议返回 null', () => {
    expect(fileUrlToLocalPath('https://example.com/a.png')).toBeNull();
    expect(fileUrlToLocalPath('/Users/test/image.png')).toBeNull();
    expect(fileUrlToLocalPath('C:\\Users\\test\\image.png')).toBeNull();
  });
});

describe('resolveAttachmentUrl', () => {
  it('file:// URL 先还原为本地路径再转资源地址', () => {
    expect(resolveAttachmentUrl('file:///Users/test/image.png')).toBe(
      converted('/Users/test/image.png'),
    );
    expect(resolveAttachmentUrl('file:///C:/Users/test/image.png')).toBe(
      converted('C:/Users/test/image.png'),
    );
    expect(resolveAttachmentUrl('file://server/share/image.png')).toBe(
      converted('\\\\server/share/image.png'),
    );
    expect(resolveAttachmentUrl('file://localhost/Users/test/image.png')).toBe(
      converted('/Users/test/image.png'),
    );
  });

  it('带 # 与 ? 的文件名转换后仍指向原文件', () => {
    expect(resolveAttachmentUrl('file:///tmp/report#1.png')).toBe(converted('/tmp/report#1.png'));
    expect(resolveAttachmentUrl('file:///tmp/report?draft.png')).toBe(
      converted('/tmp/report?draft.png'),
    );
  });

  it('大小写不敏感的 FILE:// 同样进入转换', () => {
    expect(resolveAttachmentUrl('FILE:///Users/test/image.png')).toBe(
      converted('/Users/test/image.png'),
    );
  });

  it('裸本地路径直接转换，远程与资源地址原样返回', () => {
    expect(resolveAttachmentUrl('/Users/test/image.png')).toBe(converted('/Users/test/image.png'));
    expect(resolveAttachmentUrl('C:\\Users\\test\\image.png')).toBe(
      converted('C:\\Users\\test\\image.png'),
    );
    expect(resolveAttachmentUrl('\\\\server\\share\\image.png')).toBe(
      converted('\\\\server\\share\\image.png'),
    );
    expect(resolveAttachmentUrl('https://example.com/a.png')).toBe('https://example.com/a.png');
    expect(resolveAttachmentUrl('http://example.com/a.png')).toBe('http://example.com/a.png');
    expect(resolveAttachmentUrl('asset://localhost/x.png')).toBe('asset://localhost/x.png');
    expect(resolveAttachmentUrl('about:blank')).toBe('about:blank');
  });
});

describe('clipboardImagePaths', () => {
  it('从剪贴板文本提取图片路径（file:// URL 与裸路径混排）', () => {
    expect(
      clipboardImagePaths('file:///tmp/截图 1.png\n/Users/me/b.jpg\nhttps://x/page\nhello'),
    ).toEqual(['/tmp/截图 1.png', '/Users/me/b.jpg']);
  });

  it('Windows 盘符 file URL 不丢盘符，# 文件名不截断', () => {
    expect(clipboardImagePaths('file:///C:/Users/test/pic#2.png')).toEqual([
      'C:/Users/test/pic#2.png',
    ]);
  });

  it('去除首尾引号', () => {
    expect(clipboardImagePaths('"/tmp/a.png"')).toEqual(['/tmp/a.png']);
  });
});
