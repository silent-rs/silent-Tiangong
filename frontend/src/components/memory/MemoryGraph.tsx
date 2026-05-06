//! Memory 图谱可视化组件

import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import Sigma from 'sigma';
import { Skeleton } from '@/components/ui/skeleton';
import type { MemoryNode, MemoryRelation } from '@/api/tauri';
import { memoryGraphColor, memoryTypeLabel, relationGraphColor, relationKindLabel } from './constants';

interface MemoryGraphCanvasProps {
  nodes: MemoryNode[];
  relations: MemoryRelation[];
  selectedId?: string;
  isLoading: boolean;
  onSelect: (node: MemoryNode) => void;
}

interface GraphPosition {
  x: number;
  y: number;
}

function computeMemoryGraphLayout(
  nodes: MemoryNode[],
  relations: MemoryRelation[],
  selectedId?: string,
): Map<string, GraphPosition> {
  const positions = new Map<string, GraphPosition>();
  if (nodes.length === 0) {
    return positions;
  }

  const selectedNode = nodes.find((node) => node.id === selectedId);
  const orderedNodes = selectedNode
    ? [selectedNode, ...nodes.filter((node) => node.id !== selectedNode.id)]
    : nodes;
  const visibleNodeIds = new Set(orderedNodes.map((node) => node.id));
  const nodeIndex = new Map<string, number>();
  const state = orderedNodes.map((node, index) => {
    nodeIndex.set(node.id, index);
    if (node.id === selectedId) {
      return { id: node.id, x: 0, y: 0, vx: 0, vy: 0, fixed: true };
    }
    const ringIndex = selectedNode ? index - 1 : index;
    const ringCount = selectedNode ? orderedNodes.length - 1 : orderedNodes.length;
    const angle = (Math.PI * 2 * ringIndex) / Math.max(1, ringCount) - Math.PI / 2;
    const radius = Math.max(5, Math.min(20, Math.sqrt(Math.max(1, ringCount)) * 4.4));
    return {
      id: node.id,
      x: Math.cos(angle) * radius,
      y: Math.sin(angle) * radius,
      vx: 0,
      vy: 0,
      fixed: false,
    };
  });

  const edges = relations
    .filter((relation) =>
      visibleNodeIds.has(relation.from_node_id) && visibleNodeIds.has(relation.to_node_id)
    )
    .map((relation) => ({
      source: nodeIndex.get(relation.from_node_id),
      target: nodeIndex.get(relation.to_node_id),
      weight: Math.max(0.2, Math.min(2, relation.weight || 1)),
    }))
    .filter((edge): edge is { source: number; target: number; weight: number } =>
      edge.source !== undefined && edge.target !== undefined && edge.source !== edge.target
    );

  const iterations = Math.min(120, 40 + nodes.length);
  for (let step = 0; step < iterations; step += 1) {
    const cooling = 1 - step / iterations;

    for (let i = 0; i < state.length; i += 1) {
      for (let j = i + 1; j < state.length; j += 1) {
        const a = state[i];
        const b = state[j];
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const distanceSq = Math.max(0.25, dx * dx + dy * dy);
        const distance = Math.sqrt(distanceSq);
        const force = (0.38 * cooling) / distanceSq;
        const fx = (dx / distance) * force;
        const fy = (dy / distance) * force;
        if (!a.fixed) {
          a.vx -= fx;
          a.vy -= fy;
        }
        if (!b.fixed) {
          b.vx += fx;
          b.vy += fy;
        }
      }
    }

    edges.forEach((edge) => {
      const source = state[edge.source];
      const target = state[edge.target];
      const dx = target.x - source.x;
      const dy = target.y - source.y;
      const distance = Math.max(0.5, Math.sqrt(dx * dx + dy * dy));
      const desired = 4.8 / edge.weight + (nodes.length > 60 ? 1.8 : 0);
      const force = (distance - desired) * 0.028 * edge.weight * cooling;
      const fx = (dx / distance) * force;
      const fy = (dy / distance) * force;
      if (!source.fixed) {
        source.vx += fx;
        source.vy += fy;
      }
      if (!target.fixed) {
        target.vx -= fx;
        target.vy -= fy;
      }
    });

    state.forEach((item) => {
      if (item.fixed) {
        item.x = 0;
        item.y = 0;
        item.vx = 0;
        item.vy = 0;
        return;
      }
      item.vx -= item.x * 0.006 * cooling;
      item.vy -= item.y * 0.006 * cooling;
      item.vx *= 0.82;
      item.vy *= 0.82;
      item.x += item.vx;
      item.y += item.vy;
    });
  }

  state.forEach((item) => {
    positions.set(item.id, { x: item.x, y: item.y });
  });
  return positions;
}

export function MemoryGraphCanvas({
  nodes,
  relations,
  selectedId,
  isLoading,
  onSelect,
}: MemoryGraphCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const nodeMapRef = useRef<Map<string, MemoryNode>>(new Map());

  useEffect(() => {
    const container = containerRef.current;
    if (!container || nodes.length === 0) {
      return;
    }

    const selectedNode = nodes.find((node) => node.id === selectedId);
    const orderedNodes = selectedNode
      ? [selectedNode, ...nodes.filter((node) => node.id !== selectedNode.id)]
      : nodes;
    const graph = new Graph();
    const nodeMap = new Map<string, MemoryNode>();
    const layout = computeMemoryGraphLayout(nodes, relations, selectedId);

    orderedNodes.forEach((node, index) => {
      const selected = node.id === selectedId;
      const position = layout.get(node.id) ?? { x: index * 1.5, y: 0 };
      nodeMap.set(node.id, node);
      graph.addNode(node.id, {
        label: `${node.title} · ${memoryTypeLabel(node.memory_type)}`,
        x: position.x,
        y: position.y,
        size: selected ? 18 : 11 + Math.round(node.importance * 6),
        color: selected ? '#ffffff' : memoryGraphColor(node.memory_type),
        borderColor: memoryGraphColor(node.memory_type),
        highlighted: selected,
        forceLabel: selected || nodes.length <= 24,
        zIndex: selected ? 2 : 1,
      });
    });

    const visibleNodeIds = new Set(nodes.map((node) => node.id));
    const edgeKeys = new Set<string>();
    relations.forEach((relation) => {
      if (!visibleNodeIds.has(relation.from_node_id) || !visibleNodeIds.has(relation.to_node_id)) {
        return;
      }
      const edgeKey = relation.id || `${relation.from_node_id}:${relation.to_node_id}:${relation.relation_kind}`;
      if (edgeKeys.has(edgeKey) || graph.hasEdge(edgeKey)) {
        return;
      }
      edgeKeys.add(edgeKey);
      const selected = relation.from_node_id === selectedId || relation.to_node_id === selectedId;
      graph.addDirectedEdgeWithKey(edgeKey, relation.from_node_id, relation.to_node_id, {
        label: relationKindLabel(relation.relation_kind),
        color: selected ? relationGraphColor(relation.relation_kind) : '#475569',
        size: selected ? 2.4 : 1.2,
        zIndex: selected ? 2 : 1,
      });
    });

    nodeMapRef.current = nodeMap;
    const renderer = new Sigma(graph, container, {
      allowInvalidContainer: true,
      autoCenter: true,
      autoRescale: true,
      defaultEdgeType: 'arrow',
      enableEdgeEvents: true,
      hideEdgesOnMove: false,
      hideLabelsOnMove: true,
      labelColor: { color: '#cbd5e1' },
      labelDensity: 0.08,
      labelRenderedSizeThreshold: 7,
      labelSize: 12,
      minCameraRatio: 0.45,
      maxCameraRatio: 2.6,
      renderEdgeLabels: false,
      renderLabels: true,
      stagePadding: 24,
      zIndex: true,
      nodeReducer: (nodeId, data) => {
        if (!selectedId) {
          return data;
        }
        if (nodeId === selectedId) {
          return {
            ...data,
            color: '#ffffff',
            highlighted: true,
            forceLabel: true,
            size: Math.max(data.size ?? 14, 18),
            zIndex: 3,
          };
        }
        const linked = relations.some((relation) =>
          (relation.from_node_id === selectedId && relation.to_node_id === nodeId) ||
          (relation.to_node_id === selectedId && relation.from_node_id === nodeId),
        );
        return {
          ...data,
          color: linked ? data.color : '#334155',
          forceLabel: linked && nodes.length <= 40,
          zIndex: linked ? 2 : 1,
        };
      },
      edgeReducer: (_edgeId, data) => {
        if (!selectedId) {
          return data;
        }
        const source = graph.source(_edgeId);
        const target = graph.target(_edgeId);
        const linked = source === selectedId || target === selectedId;
        return {
          ...data,
          color: linked ? data.color : '#1e293b',
          size: linked ? data.size : 0.7,
          zIndex: linked ? 2 : 1,
        };
      },
    });

    renderer.on('clickNode', ({ node }) => {
      const selected = nodeMapRef.current.get(node);
      if (selected) {
        onSelect(selected);
      }
    });

    return () => {
      renderer.kill();
      graph.clear();
    };
  }, [nodes, onSelect, relations, selectedId]);

  return (
    <div className="relative h-[420px]">
      <div ref={containerRef} className="absolute inset-0" />
      {!isLoading && nodes.length === 0 && (
        <div className="absolute inset-0 flex items-center justify-center text-sm text-muted-foreground">
          暂无匹配记忆
        </div>
      )}
      {isLoading && (
        <div className="absolute right-3 top-3 w-32 space-y-1.5 rounded-md border bg-background/80 p-2">
          <Skeleton className="h-2.5 w-full" />
          <Skeleton className="h-2.5 w-20" />
        </div>
      )}
    </div>
  );
}
