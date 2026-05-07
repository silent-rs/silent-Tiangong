//! Memory UI 常量和辅助函数

import type { MemoryCognitiveType, MemoryRelationKind } from '@/api/tauri';

// ============================================================================
// 常量定义
// ============================================================================

export const MEMORY_TYPE_OPTIONS: { value: MemoryCognitiveType; label: string }[] = [
  { value: 'factual', label: '事实性' },
  { value: 'user_preference', label: '用户偏好' },
  { value: 'user_habit', label: '用户习惯' },
  { value: 'skill', label: '技能型' },
  { value: 'project_structure', label: '项目结构' },
  { value: 'architecture_decision', label: '架构决策' },
  { value: 'problem_incident', label: '问题故障' },
  { value: 'domain_knowledge', label: '领域知识' },
];

export const MEMORY_RELATION_OPTIONS: { value: MemoryRelationKind; label: string }[] = [
  { value: 'related_to', label: '相关' },
  { value: 'depends_on', label: '依赖' },
  { value: 'supports', label: '支撑' },
  { value: 'contradicts', label: '冲突' },
  { value: 'supersedes', label: '替代' },
  { value: 'caused_by', label: '源于' },
  { value: 'belongs_to', label: '归属' },
  { value: 'learned_from', label: '学习自' },
  { value: 'validated_by', label: '验证自' },
];

// ============================================================================
// 辅助函数
// ============================================================================

export function memoryTypeLabel(value: MemoryCognitiveType): string {
  return MEMORY_TYPE_OPTIONS.find((item) => item.value === value)?.label ?? value;
}

export function relationKindLabel(value: MemoryRelationKind): string {
  return MEMORY_RELATION_OPTIONS.find((item) => item.value === value)?.label ?? value;
}

export function memoryGraphColor(memoryType: MemoryCognitiveType): string {
  switch (memoryType) {
    case 'user_preference':
      return '#3b82f6';
    case 'user_habit':
      return '#14b8a6';
    case 'skill':
      return '#22c55e';
    case 'project_structure':
      return '#f59e0b';
    case 'architecture_decision':
      return '#8b5cf6';
    case 'problem_incident':
      return '#ef4444';
    case 'domain_knowledge':
      return '#06b6d4';
    default:
      return '#64748b';
  }
}

export function relationGraphColor(relationKind: MemoryRelationKind): string {
  if (relationKind === 'contradicts' || relationKind === 'supersedes') {
    return '#ef4444';
  }
  if (relationKind === 'depends_on' || relationKind === 'caused_by') {
    return '#f59e0b';
  }
  if (relationKind === 'supports' || relationKind === 'validated_by') {
    return '#22c55e';
  }
  return '#64748b';
}

export function emptyMemoryDraft(): import('@/api/tauri').ManualMemoryDraft {
  return {
    memory_type: 'factual',
    title: '',
    summary: '',
    keywords: [],
    importance: 0.6,
  };
}
