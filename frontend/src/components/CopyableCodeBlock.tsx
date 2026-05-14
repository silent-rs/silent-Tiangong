import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";

interface CopyableCodeBlockProps {
  code: string;
  language?: string;
}

export function CopyableCodeBlock({ code, language = "text" }: CopyableCodeBlockProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch (error) {
      console.error("复制代码失败:", error);
    }
  };

  const CodeHighlighter = SyntaxHighlighter as any;

  return (
    <div className="group relative my-1.5 overflow-hidden rounded-md border border-border bg-background">
      <div className="flex h-8 items-center justify-between border-b border-border bg-muted/30 px-2">
        <span className="max-w-[12rem] truncate font-mono text-[11px] text-muted-foreground">
          {language || "text"}
        </span>
        <button
          type="button"
          onClick={handleCopy}
          className="inline-flex h-6 w-6 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          title={copied ? "已复制" : "复制代码"}
        >
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
        </button>
      </div>
      <CodeHighlighter
        style={vscDarkPlus}
        language={language || "text"}
        PreTag="div"
        className="!m-0 !rounded-none !bg-background text-xs"
        customStyle={{ padding: "12px", margin: 0, background: "transparent" }}
        codeTagProps={{
          className: "copyable-code-block__code",
          style: { background: "transparent", padding: 0 },
        }}
      >
        {code}
      </CodeHighlighter>
    </div>
  );
}
