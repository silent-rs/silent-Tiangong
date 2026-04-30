import { api } from '../api/tauri';

let permissionRequested = false;

export async function ensureDesktopNotificationPermission(): Promise<boolean> {
  try {
    if (permissionRequested) {
      return true;
    }
    permissionRequested = true;
    return await api.requestDesktopNotificationPermission();
  } catch (error) {
    console.warn('系统通知权限请求失败:', error);
    return false;
  }
}

export async function notifyBackgroundSessionCompleted(
  sessionTitle: string,
  sessionId?: string,
): Promise<void> {
  try {
    const title = '天工 - 任务完成';
    const body = `「${sessionTitle || '对话'}」执行完成`;
    const sent = await api.sendDesktopNotification(title, body, sessionId);
    if (!sent) {
      console.warn('系统通知未发送：通知权限未授予或当前平台不可用');
    }
  } catch (error) {
    console.warn('系统通知发送失败:', error);
  }
}
