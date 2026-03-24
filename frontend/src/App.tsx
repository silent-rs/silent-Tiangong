import { MainApp } from './pages/MainApp'
import { ToastProvider } from './components/Toast'
import { useTheme } from './hooks/useTheme'

function App() {
  // 初始化主题（确保 dark class 在页面加载时立即应用）
  useTheme();

  return (
    <ToastProvider>
      <MainApp />
    </ToastProvider>
  )
}

export default App
