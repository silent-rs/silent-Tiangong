import { config } from "md-editor-rt";
import type LinkifyIt from "linkify-it";
import {
  BROWSER_RENDERABLE_EXT_RE,
  isLocalFileUrl,
  toFileUrl,
} from "@/components/message/utils";

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
 *    被吞进链接文本与 href（加粗同时失去闭合）；星号后常紧跟中文
 *    （「…\*\*（分支…」到空格才停），仅靠尾部剥离会因末尾是中文而失效。
 *    校验在首个定界符（* ~ `）处截断——它们不存在于真实 URL；下划线
 *    在 URL 中段常见（foo_bar 路径），仅按 GFM autolink 尾随字节集
 *    （?!.,:*_~'）做尾部剥离。https:/ftp: 是 http: 的别名，编译时共
 *    享同一校验函数，一并生效。
 *
 * 3. 本地文件链接按「浏览器可渲染后缀」在渲染层统一裁决：模型输出里
 *    显式 Markdown 链接（[相对路径:行号](/绝对/路径.ts)）与 file: 链接
 *    的来源不可控，改写层白名单管不到它们——markdown-it 默认黑名单只
 *    拒协议（file: 在列），裸绝对路径本就放行。这里在 validateLink 层
 *    统一裁决：本地路径形态（file:、POSIX 绝对、盘符、UNC）仅当后缀
 *    浏览器可渲染时放行，其余退化为纯文本；可渲染的链接在 link_open
 *    层补正 file:// 形态并加高亮类。远程链接与危险协议仍走默认实现。
 *
 * 4. 本地文件链接高亮：file: 链接指向本机文件而非网页，行为与外链不同，
 *    link_open 渲染时追加 .md-local-file-link 类名，由样式做差异化高亮。
 *
 * 5. 裸 <a> 标签塌陷：html:true 下模型讨论渲染问题时输出的字面 <a>（无
 *    属性、常无闭合）被当作真实 HTML 开标签，HTML 解析把后续标题、列
 *    表等块级内容整个吞进一个大链接。在 html_inline 渲染层拦截裸开标
 *    签输出为字面文本——判定发生在 markdown-it 内部，代码段划分天然
 *    正确（预处理方案不可行：反引号转义嵌套会让预处理与引擎的代码段
 *    划分不一致）。带属性的真实 <a> 与孤立 </a>（HTML 解析忽略）不受
 *    影响。
 */

/** linkify-it 的 re 在类型上只暴露 RegExp 索引，src_* 模板串与惰性
 * 编译的 http 正则只存在于运行时，此处按实际结构局部断言。 */
type LinkifyRuntimeRe = {
  http?: RegExp;
  src_auth: string;
  src_host_port_strict: string;
  src_path: string;
};

/** markdown-it 解析链接时会把 url 百分号归一化（Windows 反斜杠变
 * %5C、中文变 %E4%xx…），本地路径判断与后缀裁决前先解码还原。
 * 畸形 % 序列解码会抛 URIError，原样返回交由后续默认逻辑处理。 */
function decodeLinkUrl(url: string): string {
  try {
    return decodeURI(url);
  } catch {
    return url;
  }
}

let configured = false;

export function setupMarkdownLinkify() {
  if (configured) return;
  configured = true;

  config({
    markdownItConfig(md) {
      // 本地文件链接的渲染层裁决（见头部注释 3）：只有浏览器可渲染的
      // 后缀才放行成链接，其余本地目标（源码/文档/日志等）点击也无法
      // 渲染，退化成纯文本。远程链接与危险协议走默认实现。
      const defaultValidateLink = md.validateLink.bind(md);
      md.validateLink = (url: string) => {
        const trimmed = url.trim();
        const decoded = decodeLinkUrl(trimmed);
        if (isLocalFileUrl(decoded)) {
          return BROWSER_RENDERABLE_EXT_RE.test(decoded.split(/[?#]/)[0]);
        }
        return defaultValidateLink(trimmed);
      };

      md.linkify.set({ fuzzyLink: false });

      // 存活下来的本地文件链接统一补正 file:// 形态（裸绝对路径、盘符
      // 误落主机位、反斜杠编码等）并打标记类名，供样式高亮区分于普通
      // 外链（见 index.css 的 .md-local-file-link）。渲染规则按
      // markdown-it 约定链式包装：保留既有 renderer 行为，只改 href 与
      // class。
      const defaultLinkOpen = md.renderer.rules.link_open
        ?? ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options));
      md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
        const href = tokens[idx].attrGet("href") ?? "";
        const decoded = decodeLinkUrl(href);
        if (href && isLocalFileUrl(decoded)) {
          tokens[idx].attrSet("href", toFileUrl(decoded));
          tokens[idx].attrJoin("class", "md-local-file-link");
        }
        return defaultLinkOpen(tokens, idx, options, env, self);
      };

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
          // 星号等定界符后常紧跟中文（「…\*\*（分支…」），仅尾部剥离会因
          // 末尾是中文字符整段失效：这类符号不存在于真实 URL，在首个
          // 出现处直接截断；下划线在 URL 中段常见，仅做尾部剥离。
          const mdStop = matched[0].search(/[*~`]/);
          const url = mdStop >= 0 ? matched[0].slice(0, mdStop) : matched[0];
          // GFM autolink 尾随字节集（含下划线），从尾部剥离
          return url.replace(/[?!.,:*_~']+$/, "").length;
        },
      });

      // 裸 <a> 开标签输出为字面文本（见头部注释 5）：阻止未闭合的开标签
      // 在 HTML 解析时吞掉后续块级内容。链式包装保留其余 html_inline
      // 标签的原有渲染。
      const defaultHtmlInline = md.renderer.rules.html_inline;
      md.renderer.rules.html_inline = (tokens, idx, options, env, self) => {
        if (/^<a>$/i.test(tokens[idx].content)) {
          return "&lt;a&gt;";
        }
        return defaultHtmlInline
          ? defaultHtmlInline(tokens, idx, options, env, self)
          : self.renderToken(tokens, idx, options);
      };
    },
  });
}
