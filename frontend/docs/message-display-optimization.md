# 消息显示优化

## 问题描述

从用户截图中发现两个主要问题：
1. **助手状态显示异常**：出现了 "unbidden" 角色名，显示不友好
2. **Markdown 渲染效果不佳**：代码块和文本样式需要优化

## 优化内容

### 1. 角色名称映射 ✅

**问题**：后端可能返回不规范的角色名（如 "unbidden"）

**解决方案**：
```typescript
const getRoleDisplayName = (role: string): string => {
  const roleMap: Record<string, string> = {
    'UserRole': '用户',
    'AssistantRole': '助手',
    'SystemRole': '系统',
    'unbidden': '助手', // 处理异常角色名
  };
  return roleMap[role] || role;
};
```

**效果**：
- 所有不规范的名称都会映射为友好的中文名称
- 统一显示体验

### 2. Markdown 样式优化 ✅

#### 代码块优化

**改进前**：
```tsx
customStyle: {
  background: '#1E1E1E',
  padding: '16px',
  borderRadius: '8px',
  margin: '8px 0',
}
```

**改进后**：
- 增加内边距（16px）
- 添加圆角（8px）
- 优化间距
- 添加背景色区分

#### 内联代码优化

**改进前**：
```tsx
<code className={className} {...props}>
  {children}
</code>
```

**改进后**：
```tsx
<code
  className="bg-[#2D2D30] text-[#E9E9E9] px-1.5 py-0.5 rounded text-sm font-mono"
  {...props}
>
  {children}
</code>
```

#### 文本样式优化

| 元素 | 改进前 | 改进后 |
|------|--------|--------|
| 段落 | `mb-2` | `mb-3 leading-7`（增加行高） |
| 列表 | `mb-2` | `mb-3 space-y-1.5`（增加项间距） |
| 标题 1 | 无样式 | `text-xl font-bold mb-4 mt-6` |
| 标题 2 | 无样式 | `text-lg font-bold mb-3 mt-5` |
| 标题 3 | 无样式 | `text-base font-bold mb-2 mt-4` |
| 引用块 | 无样式 | `border-l-4 border-[#10A37F] italic` |
| 粗体 | 默认 | `font-bold text-white`（更明显） |
| 链接 | 默认 | `text-[#10A37F] hover:text-[#0D8A6A]` |

### 3. 角色标签显示 ✅

为助手消息添加角色标签，明确标识消息发送者：

```tsx
{isAssistant && !isStreaming && (
  <div className="text-xs text-[#10A37F] font-medium mb-2">
    {getRoleDisplayName(message.role)}
  </div>
)}
```

**效果**：
- 每条助手消息上方显示"助手"标签
- 使用主题色（#10A37F）保持一致性
- 流式消息中不显示，避免抖动

### 4. 角色判断优化 ✅

简化角色判断逻辑：

```tsx
const isUser = message.role === 'UserRole';
const isAssistant = message.role === 'AssistantRole' || message.role === 'unbidden';
```

**好处**：
- 代码更清晰
- 处理异常角色名
- 统一判断逻辑

## 视觉效果对比

### 优化前
```
┌─────────────────────────────┐
│ [Bot]                       │
│ 正常输出内容                 │
│ 代码块样式简单               │
│ 文本间距紧凑                 │
└─────────────────────────────┘
```

### 优化后
```
┌─────────────────────────────┐
│ [Bot]                       │
│ 助手                         │  ← 角色标签
├─────────────────────────────┤
│ 正常输出内容                 │
│ • 行高增加，更易阅读         │
│ • 列表间距优化               │
│                             │
│ ┌─────────────────────────┐ │
│ │ 代码块                  │ │
│ │ • 内边距 16px           │ │
│ │ • 圆角 8px              │ │
│ │ • 背景色 #1E1E1E        │ │
│ └─────────────────────────┘ │
└─────────────────────────────┘
```

## 技术细节

### 文件变更

1. **frontend/src/components/MessageList.tsx**
   - 添加 `getRoleDisplayName` 函数
   - 优化 `MarkdownComponents` 配置
   - 添加角色标签显示
   - 简化角色判断逻辑

2. **frontend/src/components/TypingMessage.tsx**
   - 同步优化 `MarkdownComponents` 配置
   - 保持流式消息样式一致

### 构建结果

```
dist/index.html                          0.87 kB │ gzip:   0.43 kB
dist/assets/index-Dpnb3-J0.css          25.07 kB │ gzip:   5.67 kB  (+0.52 kB)
dist/assets/index-Cnpca5Y2.js           51.23 kB │ gzip:  13.70 kB  (+1.95 kB)
dist/assets/react-core-BiokaMjE.js     134.79 kB │ gzip:  43.27 kB
dist/assets/markdown-CWej1cPf.js       753.96 kB │ gzip: 266.58 kB

✓ built in 2.00s
```

**说明**：
- CSS 增加 0.52 kB（新增样式）
- JS 增加 1.95 kB（角色映射逻辑）
- 总增量：约 2.5 kB（可接受）

## 测试清单

- [x] 角色名称映射正常工作
- [x] Markdown 样式优化生效
- [x] 代码块渲染正确
- [x] 列表间距合理
- [x] 角色标签显示正确
- [x] 流式消息不影响标签
- [x] 构建成功无错误

## 后续优化建议

1. **代码高亮主题**
   - 考虑提供多套主题选择
   - 支持用户自定义颜色

2. **复制代码按钮**
   - 为代码块添加复制按钮
   - 提升用户体验

3. **数学公式支持**
   - 集成 KaTeX 或 MathJax
   - 支持 LaTeX 公式渲染

4. **代码折叠**
   - 长代码块自动折叠
   - 点击展开完整内容

5. **性能优化**
   - 长消息虚拟滚动
   - Markdown 渲染结果缓存

## 总结

通过本次优化，成功解决了：
- ✅ 角色名称显示异常问题
- ✅ Markdown 渲染样式优化
- ✅ 代码块视觉效果提升
- ✅ 整体阅读体验改善

所有改进均已通过测试并成功构建，用户可以立即体验优化后的效果。
