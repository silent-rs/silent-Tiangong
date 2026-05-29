import { useEffect, useState, useRef } from 'react';
import { MdPreview } from 'md-editor-rt';
import 'md-editor-rt/lib/preview.css';

import { ThinkingBlock } from './ThinkingBlock';
import { useTheme } from '@/hooks/useTheme';

interface TypingMessageProps {
  content: string;
  reasoningContent?: string;
  speed?: number;
  onComplete?: () => void;
}

export function TypingMessage({ content, reasoningContent, speed: _speed = 300, onComplete }: TypingMessageProps) {
  const [isComplete, setIsComplete] = useState(false);
  const prevContentRef = useRef('');
  const { resolvedTheme } = useTheme();

  useEffect(() => {
    if (!content) {
      setIsComplete(true);
      onComplete?.();
      return;
    }

    if (content !== prevContentRef.current) {
      prevContentRef.current = content;
      setIsComplete(false);
    }
  }, [content, onComplete]);

  return (
    <div>
      {reasoningContent && (
        <ThinkingBlock content={reasoningContent} defaultExpanded={false} />
      )}

      <MdPreview modelValue={content} theme={resolvedTheme} />

      {!isComplete && content.length > 0 && (
        <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />
      )}
    </div>
  );
}
