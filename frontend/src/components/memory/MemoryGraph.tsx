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
  onClearSelection: () => void;
}

interface GraphPosition {
  x: number;
  y: number;
}

function computeNodeRelationStats(
  nodes: MemoryNode[],
  relations: MemoryRelation[],
): Map<string, { degree: number; weight: number }> {
  const visibleNodeIds = new Set(nodes.map((node) => node.id));
  const stats = new Map<string, { degree: number; weight: number }>();
  nodes.forEach((node) => {
    stats.set(node.id, { degree: 0, weight: 0 });
  });

  relations.forEach((relation) => {
    if (!visibleNodeIds.has(relation.from_node_id) || !visibleNodeIds.has(relation.to_node_id)) {
      return;
    }
    const weight = Math.max(0.2, Math.min(2, relation.weight || 1));
    const from = stats.get(relation.from_node_id);
    const to = stats.get(relation.to_node_id);
    if (from) {
      from.degree += 1;
      from.weight += weight;
    }
    if (to) {
      to.degree += 1;
      to.weight += weight;
    }
  });

  return stats;
}

function memoryGraphNodeSize(degree: number, selected: boolean, nodeCount: number): number {
  const degreeBaseSize = [8, 13, 19, 25, 32][Math.min(degree, 4)];
  const extraDegreeSize = Math.max(0, degree - 4) * 1.6;
  const densityScale = Math.max(0.55, Math.min(1, Math.sqrt(140 / Math.max(140, nodeCount))));
  const size = (degreeBaseSize + extraDegreeSize) * densityScale;
  return selected ? Math.max(size + 4, 22) : Math.min(40, Math.max(6, size));
}

function memoryGraphNodeRadius(degree: number, nodeCount: number): number {
  return memoryGraphNodeSize(degree, false, nodeCount) * 0.16;
}

function memoryGraphNodeColor(node: MemoryNode): string {
  const color = memoryGraphColor(node.memory_type);
  const alpha = Math.round((0.36 + Math.max(0, Math.min(1, node.importance || 0)) * 0.64) * 255);
  return `${color}${alpha.toString(16).padStart(2, '0')}`;
}

function computeMemoryGraphLayout(
  nodes: MemoryNode[],
  relations: MemoryRelation[],
  relationStats: Map<string, { degree: number; weight: number }>,
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
      return {
        id: node.id,
        x: 0,
        y: 0,
        vx: 0,
        vy: 0,
        radius: memoryGraphNodeRadius(relationStats.get(node.id)?.degree ?? 0, nodes.length),
        fixed: true,
      };
    }
    const ringIndex = selectedNode ? index - 1 : index;
    const ringCount = selectedNode ? orderedNodes.length - 1 : orderedNodes.length;
    const angle = (Math.PI * 2 * ringIndex) / Math.max(1, ringCount) - Math.PI / 2;
    const radius = Math.max(8, Math.min(36, Math.sqrt(Math.max(1, ringCount)) * 5.6));
    return {
      id: node.id,
      x: Math.cos(angle) * radius,
      y: Math.sin(angle) * radius,
      vx: 0,
      vy: 0,
      radius: memoryGraphNodeRadius(relationStats.get(node.id)?.degree ?? 0, nodes.length),
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

  const iterations = Math.min(80, 32 + Math.ceil(Math.sqrt(nodes.length) * 6));
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

        const minDistance = a.radius + b.radius + (nodes.length > 80 ? 0.8 : 1.2);
        if (distance < minDistance) {
          const overlapForce = (minDistance - distance) * 0.08 * cooling;
          const ox = (dx / distance) * overlapForce;
          const oy = (dy / distance) * overlapForce;
          if (!a.fixed) {
            a.vx -= ox;
            a.vy -= oy;
          }
          if (!b.fixed) {
            b.vx += ox;
            b.vy += oy;
          }
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

  for (let step = 0; step < 18; step += 1) {
    for (let i = 0; i < state.length; i += 1) {
      for (let j = i + 1; j < state.length; j += 1) {
        const a = state[i];
        const b = state[j];
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const distance = Math.max(0.001, Math.sqrt(dx * dx + dy * dy));
        const minDistance = a.radius + b.radius + (nodes.length > 80 ? 0.6 : 1);
        if (distance >= minDistance) {
          continue;
        }
        const shift = (minDistance - distance) * 0.52;
        const sx = (dx / distance) * shift;
        const sy = (dy / distance) * shift;
        if (!a.fixed && !b.fixed) {
          a.x -= sx * 0.5;
          a.y -= sy * 0.5;
          b.x += sx * 0.5;
          b.y += sy * 0.5;
        } else if (!a.fixed) {
          a.x -= sx;
          a.y -= sy;
        } else if (!b.fixed) {
          b.x += sx;
          b.y += sy;
        }
      }
    }
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
  onClearSelection,
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
    const relationStats = computeNodeRelationStats(nodes, relations);
    const layout = computeMemoryGraphLayout(nodes, relations, relationStats, selectedId);
    const selectedLinkedNodeIds = new Set<string>();
    if (selectedId) {
      relations.forEach((relation) => {
        if (relation.from_node_id === selectedId) {
          selectedLinkedNodeIds.add(relation.to_node_id);
        }
        if (relation.to_node_id === selectedId) {
          selectedLinkedNodeIds.add(relation.from_node_id);
        }
      });
    }

    orderedNodes.forEach((node, index) => {
      const selected = node.id === selectedId;
      const position = layout.get(node.id) ?? { x: index * 1.5, y: 0 };
      const degree = relationStats.get(node.id)?.degree ?? 0;
      const size = memoryGraphNodeSize(degree, selected, nodes.length);
      nodeMap.set(node.id, node);
      graph.addNode(node.id, {
        label: `${node.title} · ${memoryTypeLabel(node.memory_type)}`,
        x: position.x,
        y: position.y,
        size,
        color: selected ? '#ffffff' : memoryGraphNodeColor(node),
        borderColor: memoryGraphColor(node.memory_type),
        highlighted: selected,
        forceLabel: false,
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
    let hoveredNodeId: string | undefined;
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
      labelRenderedSizeThreshold: Number.POSITIVE_INFINITY,
      labelSize: 12,
      minCameraRatio: 0.45,
      maxCameraRatio: 2.6,
      renderEdgeLabels: false,
      renderLabels: true,
      stagePadding: 24,
      zIndex: true,
      nodeReducer: (nodeId, data) => {
        const hovered = nodeId === hoveredNodeId;
        const linked = Boolean(selectedId && selectedLinkedNodeIds.has(nodeId));
        const labelVisible = hovered || nodeId === selectedId || linked;
        if (!selectedId) {
          return {
            ...data,
            highlighted: hovered,
            forceLabel: hovered,
            zIndex: hovered ? 3 : data.zIndex,
          };
        }
        if (nodeId === selectedId) {
          return {
            ...data,
            color: '#ffffff',
            highlighted: true,
            forceLabel: labelVisible,
            size: Math.max(data.size ?? 14, 20),
            zIndex: 3,
          };
        }
        return {
          ...data,
          color: linked || hovered ? data.color : '#33415566',
          highlighted: hovered,
          forceLabel: labelVisible,
          zIndex: hovered ? 3 : linked ? 2 : 1,
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
    renderer.on('clickStage', onClearSelection);
    renderer.on('enterNode', ({ node }) => {
      hoveredNodeId = node;
      renderer.refresh();
    });
    renderer.on('leaveNode', ({ node }) => {
      if (hoveredNodeId === node) {
        hoveredNodeId = undefined;
        renderer.refresh();
      }
    });

    return () => {
      renderer.kill();
      graph.clear();
    };
  }, [nodes, onClearSelection, onSelect, relations, selectedId]);

  return (
    <div className="relative h-full min-h-[360px]">
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
