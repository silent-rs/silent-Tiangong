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
 *    被吞进链接文本与 href（加粗同时失去闭合）；星号后常紧跟中文
 *    （「…\*\*（分支…」到空格才停），仅靠尾部剥离会因末尾是中文而失效。
 *    校验在首个定界符（* ~ `）处截断——它们不存在于真实 URL；下划线
 *    在 URL 中段常见（foo_bar 路径），仅按 GFM autolink 尾随字节集
 *    （?!.,:*_~'）做尾部剥离。https:/ftp: 是 http: 的别名，编译时共
 *    享同一校验函数，一并生效。
 *
 * validate 复刻 linkify-it http: 默认实现（惰性编译 re.http 后取匹配长
 * 度），在返回长度前追加定界符截断与尾部剥离。
 *
 * 3. file: 链接被整段丢弃：markdown-it 默认 validateLink 把 file: 与
 *    javascript:/vbscript:/data: 一并列入黑名单，校验失败的链接不生成
 *    <a>，Markdown 语法原样退化成文本（本地 html 路径改写后显示成
 *    「[`/…/x.html`](file:///…/x.html)」）。这里只额外放行 file:，其余
 *    危险协议仍交给默认实现拒绝；渲染出的链接由消息列表的点击拦截接管，
 *    走嵌入浏览器打开，不会真正导航 webview。
 *
 * 4. 本地文件链接高亮：file: 链接指向本机文件而非网页，行为与外链不同，
 *    link_open 渲染时追加 .md-local-file-link 类名，由样式做差异化高亮。
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
      // 放行 file: 协议：默认 validateLink 与 javascript:/vbscript:/data:
      // 一同拒绝它，导致本地文件链接退化成裸文本。其余协议仍走默认判断。
      // POSIX（file:///Users/…）与 Windows（file:///C:/…、file://server/share/…）
      // 形态一致，只按 scheme 判断即可。
      const defaultValidateLink = md.validateLink.bind(md);
      md.validateLink = (url: string) =>
        /^file:/i.test(url.trim()) || defaultValidateLink(url);

      md.linkify.set({ fuzzyLink: false });

      // 本地文件链接打标记类名，供样式高亮区分于普通外链（见
      // index.css 的 .md-local-file-link）。渲染规则按 markdown-it 约定
      // 链式包装：保留既有 renderer 行为，只追加 class。
      const defaultLinkOpen = md.renderer.rules.link_open
        ?? ((tokens, idx, options, _env, self) => self.renderToken(tokens, idx, options));
      md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
        const href = tokens[idx].attrGet("href") ?? "";
        if (/^file:/i.test(href.trim())) {
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
          // 末尾是中文字符而整段失效：这类符号不存在于真实 URL，在首个
          // 出现处直接截断；下划线在 URL 中段常见，仅做尾部剥离。
          const mdStop = matched[0].search(/[*~`]/);
          const url = mdStop >= 0 ? matched[0].slice(0, mdStop) : matched[0];
          // GFM autolink 尾随字节集（含下划线），从尾部剥离
          return url.replace(/[?!.,:*_~']+$/, "").length;
        },
      });
    },
  });
}
