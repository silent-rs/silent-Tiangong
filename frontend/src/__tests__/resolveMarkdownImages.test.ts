import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://mocked/${path}`,
}));

import { resolveMarkdownImages } from '@/components/message/utils';

const converted = (path: string) => `asset://mocked/${path}`;

describe('resolveMarkdownImages', () => {
  it('本地 SVG 文件链接改写为内联图片并转换资源地址', () => {
    expect(
      resolveMarkdownImages('已生成 [图](file:///Users/test/pelican.svg) 请查收'),
    ).toBe(`已生成 ![图](${converted('/Users/test/pelican.svg')}) 请查收`);
  });

  it('POSIX 裸路径与 Windows 盘符的 SVG 链接同样内联', () => {
    expect(resolveMarkdownImages('[a](/tmp/diagram.svg)')).toBe(
      `![a](${converted('/tmp/diagram.svg')})`,
    );
    expect(resolveMarkdownImages('[a](C:\\Users\\test\\diagram.svg)')).toBe(
      `![a](${converted('C:\\Users\\test\\diagram.svg')})`,
    );
  });

  it('已是图片语法的 SVG 只做路径转换，不重复加感叹号', () => {
    expect(resolveMarkdownImages('![icon](file:///tmp/icon.svg)')).toBe(
      `![icon](${converted('/tmp/icon.svg')})`,
    );
  });

  it('远程 SVG 链接与非 SVG 链接保持原样', () => {
    expect(resolveMarkdownImages('[logo](https://example.com/logo.svg)')).toBe(
      '[logo](https://example.com/logo.svg)',
    );
    expect(resolveMarkdownImages('[文档](file:///tmp/readme.md)')).toBe(
      '[文档](file:///tmp/readme.md)',
    );
  });

  it('扩展名大小写不敏感', () => {
    expect(resolveMarkdownImages('[a](file:///tmp/A.SVG)')).toBe(
      `![a](${converted('/tmp/A.SVG')})`,
    );
  });

  it('既有位图图片路径仍转换资源地址', () => {
    expect(resolveMarkdownImages('![photo](/Users/test/p.png)')).toBe(
      `![photo](${converted('/Users/test/p.png')})`,
    );
  });
});
