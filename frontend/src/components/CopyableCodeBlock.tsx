import { useState, useEffect, useRef } from "react";
import { Check, Copy } from "lucide-react";

interface CopyableCodeBlockProps {
  code: string;
  language?: string;
}

export function CopyableCodeBlock({ code, language = "text" }: CopyableCodeBlockProps) {
  const [copied, setCopied] = useState(false);
  const [isVisible, setIsVisible] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      ([entry]) => setIsVisible(entry.isIntersecting),
      { rootMargin: "300px" }
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch (error) {
      console.error("复制代码失败:", error);
    }
  };

  return (
    <div ref={containerRef} className="group relative my-1.5 overflow-hidden rounded-md border border-border bg-background">
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
      {isVisible ? (
        <SyntaxHighlighterLazy code={code} language={language} />
      ) : (
        <pre className="p-3 m-0 overflow-x-auto text-xs leading-relaxed">
          <code>{code}</code>
        </pre>
      )}
    </div>
  );
}

/** 延迟加载 SyntaxHighlighter，只在可见时才引入重量级依赖 */
function SyntaxHighlighterLazy({ code, language }: { code: string; language: string }) {
  const [mod, setMod] = useState<{ SyntaxHighlighter: any; style: any } | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      import("react-syntax-highlighter/dist/esm/prism"),
      import("react-syntax-highlighter/dist/esm/styles/prism"),
    ]).then(([prismMod, stylesMod]) => {
      if (cancelled) return;
      setMod({
        SyntaxHighlighter: (prismMod as any).default || prismMod,
        style: (stylesMod as any).vscDarkPlus,
      });
    });
    return () => { cancelled = true; };
  }, []);

  if (!mod) {
    return (
      <pre className="p-3 m-0 overflow-x-auto text-xs leading-relaxed">
        <code>{code}</code>
      </pre>
    );
  }

  const { SyntaxHighlighter, style } = mod;
  return (
    <SyntaxHighlighter
      style={style}
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
    </SyntaxHighlighter>
  );
}
