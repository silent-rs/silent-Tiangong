# 图片生成指南

## 📷 您的图片需求

**场景描述**：女孩在图书馆的窗户前，看向远方

**推荐提示词（中英文）**：
- 中文：一个年轻女孩站在图书馆古老的窗户前，阳光透过窗户洒进来，她望向远方的地平线，充满希望和憧憬，温暖的光线，复古的图书馆氛围，油画风格，高细节
- 英文：A young girl standing in front of an ancient library window, sunlight streaming through the window, she gazes into the distant horizon with hope and longing, warm lighting, vintage library atmosphere, oil painting style, highly detailed

## 🎨 推荐图片生成服务

### 1. DALL-E 3 (OpenAI)

**特点**：
- 文本理解能力强，能准确还原复杂场景
- 支持多种艺术风格
- 图片质量高，细节丰富

**使用方式**：
- 网页版：https://chat.openai.com
- API 调用：通过 OpenAI API

**推荐提示词**：
```
A young girl standing in front of a vintage library window, sunlight streaming through, gazing at the distant horizon with hope, warm golden lighting, old books in background, dreamy atmosphere, highly detailed, artistic style
```

### 2. Midjourney

**特点**：
- 艺术风格独特，画面美感强
- 支持丰富的参数调节
- 社区活跃，灵感丰富

**使用方式**：
- Discord 平台：https://discord.gg/midjourney
- 命令格式：`/imagine prompt: [描述]`

**推荐提示词**：
```
/imagine prompt: a young girl by the library window, looking into the distance, sunlight streaming through vintage windows, old books, warm atmosphere, nostalgic mood, cinematic lighting, highly detailed, artstation quality --ar 16:9 --v 6
```

**参数说明**：
- `--ar 16:9`：宽高比 16:9
- `--v 6`：使用第 6 版本

### 3. Stable Diffusion

**特点**：
- 开源免费
- 可本地部署
- 支持多种模型和插件

**使用方式**：
- 网页版：https://stability.ai
- 本地部署：ComfyUI / Automatic1111

**推荐提示词**：
```
Positive: (masterpiece, best quality, highly detailed:1.2), a young girl standing by a library window, looking into the distance, vintage library interior, sunlight through window, warm golden hour lighting, nostalgic atmosphere, oil painting style, artstation trending

Negative: low quality, blurry, distorted, ugly, bad anatomy, watermark, signature
```

### 4. Leonardo.AI

**特点**：
- 免费额度充足
- 多种预设模型
- 支持风格迁移

**使用方式**：
- 网页版：https://leonardo.ai

**推荐设置**：
- 模型：Leonardo Phoenix
- 风格：Cinematic / Artistic

## 💡 提示词优化技巧

### 关键元素组合：
1. **主体**：young girl（年轻女孩）
2. **场景**：library window（图书馆窗户）
3. **动作**：gazing into the distance（看向远方）
4. **氛围**：warm sunlight, nostalgic（温暖阳光，怀旧）
5. **风格**：artistic, highly detailed（艺术感，高细节）

### 质量增强词：
- masterpiece（杰作）
- best quality（最佳质量）
- highly detailed（高细节）
- 8k resolution（8K 分辨率）
- artstation quality（ArtStation 级别质量）

### 光线描述：
- golden hour（黄金时刻）
- sunlight streaming（阳光洒入）
- soft lighting（柔和光线）
- warm atmosphere（温暖氛围）

## 🎯 针对不同平台的优化建议

### DALL-E 3
- 使用自然语言描述
- 强调情感和氛围
- 可以添加具体的艺术风格

### Midjourney
- 使用逗号分隔关键词
- 添加 `--ar` 参数设置宽高比
- 使用 `--v` 指定版本

### Stable Diffusion
- 使用权重语法 `(关键词:权重值)`
- 添加负向提示词过滤不良元素
- 可以使用特定模型（如写实、动漫风格）

## 📝 推荐生成流程

1. **首次尝试**：使用基础提示词生成
2. **迭代优化**：根据结果调整细节描述
3. **风格调整**：尝试不同的艺术风格
4. **参数微调**：调整光线、色调、构图等

## 🔗 快速链接

- DALL-E: https://chat.openai.com
- Midjourney: https://discord.gg/midjourney
- Stable Diffusion: https://stability.ai
- Leonardo.AI: https://leonardo.ai

---

**创建时间**：2024年
**用途**：图片生成参考指南