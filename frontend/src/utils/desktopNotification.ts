import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification';

let permissionRequested = false;

export async function ensureDesktopNotificationPermission(): Promise<boolean> {
  try {
    if (await isPermissionGranted()) {
      return true;
    }

    if (permissionRequested) {
      return false;
    }
    permissionRequested = true;

    const permission = await requestPermission();
    return permission === 'granted';
  } catch (error) {
    console.warn('系统通知权限请求失败:', error);
    return false;
  }
}

export async function notifyBackgroundSessionCompleted(sessionTitle: string): Promise<void> {
  const granted = await ensureDesktopNotificationPermission();
  if (!granted) {
    return;
  }

  try {
    sendNotification({
      title: '天工 - 任务完成',
      body: `「${sessionTitle || '对话'}」执行完成`,
      group: 'tiangong-background-sessions',
      autoCancel: true,
    });
  } catch (error) {
    console.warn('系统通知发送失败:', error);
  }
}
