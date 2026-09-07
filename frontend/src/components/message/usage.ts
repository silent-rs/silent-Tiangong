import type { TokenUsage } from "@/api/tauri";

export function cacheHitRate(usage: TokenUsage): number | null {
  const hit = usage.prompt_cache_hit_tokens;
  const miss = usage.prompt_cache_miss_tokens;
  if (hit == null || miss == null || usage.prompt_tokens <= 0 || hit + miss !== usage.prompt_tokens) return null;
  return hit / usage.prompt_tokens;
}

export function sumUsage(usages: TokenUsage[]): TokenUsage {
  const total: TokenUsage = { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 };
  for (const usage of usages) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.total_tokens += usage.total_tokens;
    if (usage.prompt_cache_hit_tokens != null) total.prompt_cache_hit_tokens = (total.prompt_cache_hit_tokens ?? 0) + usage.prompt_cache_hit_tokens;
    if (usage.prompt_cache_miss_tokens != null) total.prompt_cache_miss_tokens = (total.prompt_cache_miss_tokens ?? 0) + usage.prompt_cache_miss_tokens;
  }
  total.cache_hit_rate = cacheHitRate(total);
  return total;
}
