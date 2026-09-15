import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import { setupMarkdownLinkify } from './utils/markdownLinkify'
import App from './App'

// 必须先于任何 Markdown 预览挂载执行（修正链接识别，全局一次）
setupMarkdownLinkify()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
