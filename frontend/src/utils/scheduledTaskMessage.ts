const SCHEDULED_TASK_HEADER = '[定时任务触发]';
const TASK_NAME_PREFIX = '任务名称：';
const TASK_DESCRIPTION_PREFIX = '任务描述：';

interface MessageLine {
  text: string;
  start: number;
  next: number;
}

export interface ScheduledTaskMessage {
  name: string;
  description: string;
  payload: string;
  offsets: {
    name: number;
    description: number;
    payload: number;
  };
}

function readLine(message: string, start: number): MessageLine | null {
  const newline = message.indexOf('\n', start);
  if (newline < 0) return null;

  const end = message[newline - 1] === '\r' ? newline - 1 : newline;
  const text = message.slice(start, end);
  if (text.includes('\r')) return null;

  return { text, start, next: newline + 1 };
}

/** 仅识别调度器生成的完整消息格式，避免普通用户消息被误判。 */
export function parseScheduledTaskMessage(message: string): ScheduledTaskMessage | null {
  const headerLine = readLine(message, 0);
  if (!headerLine || headerLine.text !== SCHEDULED_TASK_HEADER) return null;

  const nameLine = readLine(message, headerLine.next);
  if (!nameLine || !nameLine.text.startsWith(TASK_NAME_PREFIX)) return null;

  const descriptionLine = readLine(message, nameLine.next);
  if (!descriptionLine || !descriptionLine.text.startsWith(TASK_DESCRIPTION_PREFIX)) return null;

  const blankLine = readLine(message, descriptionLine.next);
  if (!blankLine || blankLine.text !== '') return null;

  return {
    name: nameLine.text.slice(TASK_NAME_PREFIX.length),
    description: descriptionLine.text.slice(TASK_DESCRIPTION_PREFIX.length),
    payload: message.slice(blankLine.next),
    offsets: {
      name: nameLine.start + TASK_NAME_PREFIX.length,
      description: descriptionLine.start + TASK_DESCRIPTION_PREFIX.length,
      payload: blankLine.next,
    },
  };
}
