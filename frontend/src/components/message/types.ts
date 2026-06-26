import type { ContentBlock } from "@/api/tauri";

export interface MessageItem {
  id: string;
  role: "system" | "user" | "assistant" | "tool";
  content: ContentBlock[];
  reasoning_content: string;
  worker_id?: string;
  media?: {
    kind: "image" | "video" | "audio" | "file";
    url: string;
    mime_type?: string;
    title?: string;
    capability?: string;
  }[];
  tool_calls?: { id: string; name: string; arguments?: unknown }[];
  tool_call_id?: string;
  tool_name?: string;
  tool_result_is_error?: boolean;
  compact?: boolean;
  phase?: "normal" | "react" | "summary";
  created_at: string;
}

export interface MessageGroup {
  key: string;
  type: "user" | "agent_turn" | "worker";
  worker_id?: string;
  messages: MessageItem[];
}

export interface SystemMessageMeta {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  summary: string;
  toolName?: string;
}
