import { MainApp } from './pages/MainApp'
import { ToastProvider } from './components/Toast'

function App() {
  return (
    <ToastProvider>
      <MainApp />
    </ToastProvider>
  )
}

export default App
