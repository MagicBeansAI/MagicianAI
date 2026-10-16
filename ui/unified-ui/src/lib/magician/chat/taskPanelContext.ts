/**
 * Svelte context key for the chat-surface "open the execution panel for
 * this task id" handler.
 *
 * `ChatPanel` provides it (setContext) backed by its `openTaskPanel()`,
 * which opens the chat-owned `<TaskPanelDrawer>` in place and resolves the
 * task by id — including Internal tasks the `/tasks` feed never loads.
 * `ChatMarkdown` consumes it (getContext) so a task id the LLM mentions in
 * prose opens that panel instead of navigating to `/tasks?selected=<id>`
 * (which can't resolve Internal tasks and dead-ends on an empty pane).
 *
 * Undefined when `ChatMarkdown` renders outside a chat surface — consumers
 * then fall back to normal href navigation.
 */
export const CHAT_TASK_PANEL_OPENER = Symbol('chat-task-panel-opener');

export type ChatTaskPanelOpener = (taskId: string) => void;
