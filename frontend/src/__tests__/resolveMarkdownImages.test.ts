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

describe('本地可渲染文件行内代码链接化', () => {
  it('POSIX 绝对路径改写为 file:// 链接并保留反引号', () => {
    expect(
      resolveMarkdownImages('文件位置：`/Users/hubertshelley/Documents/organoid-intro.html`（单文件）'),
    ).toBe('文件位置：[`/Users/hubertshelley/Documents/organoid-intro.html`](file:///Users/hubertshelley/Documents/organoid-intro.html)（单文件）');
  });

  it('file:// 前缀、Windows 盘符与 .htm 后缀同样改写', () => {
    expect(resolveMarkdownImages('`file:///tmp/a.html`')).toBe(
      '[`file:///tmp/a.html`](file:///tmp/a.html)',
    );
    expect(resolveMarkdownImages('`C:\\Users\\test\\b.htm`')).toBe(
      '[`C:\\Users\\test\\b.htm`](file:///C:/Users/test/b.htm)',
    );
  });

  it('Windows 反斜杠形态的 file: URL 与 UNC 路径规范化', () => {
    // 盘符误落主机位：WebView 会把 C: 当主机名，补正为三斜杠
    expect(resolveMarkdownImages('`file://C:/Users/test/b.html`')).toBe(
      '[`file://C:/Users/test/b.html`](file:///C:/Users/test/b.html)',
    );
    expect(resolveMarkdownImages('`file://C:\\Users\\test\\b.html`')).toBe(
      '[`file://C:\\Users\\test\\b.html`](file:///C:/Users/test/b.html)',
    );
    // UNC：主机位落于 file://server
    expect(resolveMarkdownImages('`\\\\server\\share\\a.html`')).toBe(
      '[`\\\\server\\share\\a.html`](file://server/share/a.html)',
    );
  });

  it('Windows 路径中的空白与中文同样编码', () => {
    expect(resolveMarkdownImages('`C:\\我的 文件\\a.html`')).toBe(
      '[`C:\\我的 文件\\a.html`](file:///C:/%E6%88%91%E7%9A%84%20%E6%96%87%E4%BB%B6/a.html)',
    );
  });

  it('含空白中文的路径编码后写入链接', () => {
    expect(resolveMarkdownImages('`/Users/我的 文件/a.html`')).toBe(
      '[`/Users/我的 文件/a.html`](file:///Users/%E6%88%91%E7%9A%84%20%E6%96%87%E4%BB%B6/a.html)',
    );
  });

  it('浏览器可渲染的矢量图、位图与 PDF 同样改写', () => {
    expect(resolveMarkdownImages('`/tmp/diagram.svg`')).toBe(
      '[`/tmp/diagram.svg`](file:///tmp/diagram.svg)',
    );
    expect(resolveMarkdownImages('`/Users/test/photo.jpeg`')).toBe(
      '[`/Users/test/photo.jpeg`](file:///Users/test/photo.jpeg)',
    );
    expect(resolveMarkdownImages('`/Users/test/report.PDF`')).toBe(
      '[`/Users/test/report.PDF`](file:///Users/test/report.PDF)',
    );
  });

  it('浏览器无法渲染的类型保持原样', () => {
    expect(resolveMarkdownImages('`/tmp/notes.md`')).toBe('`/tmp/notes.md`');
    expect(resolveMarkdownImages('`/tmp/app.log`')).toBe('`/tmp/app.log`');
    expect(resolveMarkdownImages('`/tmp/main.rs`')).toBe('`/tmp/main.rs`');
    expect(resolveMarkdownImages('`/tmp/doc.docx`')).toBe('`/tmp/doc.docx`');
  });

  it('相对路径与非本地绝对路径行内代码保持原样', () => {
    expect(resolveMarkdownImages('`target/x.html`')).toBe('`target/x.html`');
    expect(resolveMarkdownImages('`/tmp/readme.md`')).toBe('`/tmp/readme.md`');
  });

  it('代码块内的路径原样展示，不链接化', () => {
    const md = '示例：\n```\n`/Users/demo/index.html`\n```';
    expect(resolveMarkdownImages(md)).toBe(md);
  });
});
