import { config } from "md-editor-rt";
import type LinkifyIt from "linkify-it";

/**
 * 消息 Markdown 预览的链接识别修正（作用于全部 MdPreview 实例，须在
 * 首个预览挂载前调用，应用入口执行一次）。
 *
 * 1. 文件名误识别为链接：linkify 默认开启 fuzzyLink，把「单词.顶级域」
 *    样式的裸词识别为无协议链接——文件后缀大量撞真实 TLD（.rs/.sh/.py/
 *    .name/.status/.io…），slots.rs、manifest.name、session.input-status
 *    均被链化。关闭后无协议前缀不再链化，带 http(s):// 的完整 URL 与
 *    邮箱识别（fuzzyEmail）不受影响。
 *
 * 2. URL 吞并尾部 Markdown 定界符：默认终止字符集（空白、括号引号、
 *    .,;:?!"- 等）不含 Markdown 符号，「https://…\*\*加粗\*\*」的星号
 *    被吞进链接文本与 href，加粗也因此失去闭合。按 GFM autolink 规范
 *    的尾随字节集（?!.,:*_~'）包装默认校验做尾部剥离，剥离后的星号回
 *    到正文参与行内解析。
 *
 * validate 复刻 linkify-it http: 默认实现（惰性编译 re.http 后取匹配长
 * 度），仅在返回长度前追加尾部剥离；https:/ftp: 是 http: 的别名，编译
 * 时共享同一校验函数，一并生效。
 */

/** linkify-it 的 re 在类型上只暴露 RegExp 索引，src_* 模板串与惰性
 * 编译的 http 正则只存在于运行时，此处按实际结构局部断言。 */
type LinkifyRuntimeRe = {
  http?: RegExp;
  src_auth: string;
  src_host_port_strict: string;
  src_path: string;
};

let configured = false;

export function setupMarkdownLinkify() {
  if (configured) return;
  configured = true;

  config({
    markdownItConfig(md) {
      md.linkify.set({ fuzzyLink: false });

      md.linkify.add("http:", {
        validate(text: string, pos: number, self: LinkifyIt) {
          const re = self.re as unknown as LinkifyRuntimeRe;
          const tail = text.slice(pos);
          if (!re.http) {
            re.http = new RegExp(
              `^\\/\\/${re.src_auth}${re.src_host_port_strict}${re.src_path}`,
              "i",
            );
          }
          const matched = tail.match(re.http);
          if (!matched) return 0;
          // GFM autolink 尾随字节集，从匹配尾部剥离
          return matched[0].replace(/[?!.,:*_~']+$/, "").length;
        },
      });
    },
  });
}
