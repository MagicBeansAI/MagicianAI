import type {
  TaskStatusUpdateRender,
  ToolCallExecutedRender,
} from "./types.js";

export function formatToolCallExecuted(
  toolName: string,
  summary: string,
): string {
  return `Action completed: ${toolName}\n${summary}`;
}

export function formatTaskStatusUpdate(
  taskId: string,
  status: string,
  summary?: string,
): string {
  return summary
    ? `Task ${taskId}: ${status}\n${summary}`
    : `Task ${taskId}: ${status}`;
}

export function buildToolCallExecutedRender(
  sessionId: string,
  messageId: string,
  toolName: string,
  summary: string,
): ToolCallExecutedRender {
  return {
    sessionId,
    messageId,
    toolName,
    summary,
    text: formatToolCallExecuted(toolName, summary),
  };
}

export function buildTaskStatusUpdateRender(
  sessionId: string,
  messageId: string,
  taskId: string,
  status: string,
  summary?: string,
): TaskStatusUpdateRender {
  return {
    sessionId,
    messageId,
    taskId,
    status,
    ...(summary ? { summary } : {}),
    text: formatTaskStatusUpdate(taskId, status, summary),
  };
}
