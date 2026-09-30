/** Agent option for task creation assignment controls. */
export interface AgentPickerOption {
	agent_id: string;
	name: string;
}

export interface ParsedTaskAction {
	taskId: string;
	action:
		| 'open'
		| 'doit'
		| 'abort'
		| 'cancel'
		| 'delete'
		| 'reset'
		| 'schedule_today'
		| 'schedule_tomorrow'
		| 'schedule_next_week'
		| 'schedule_clear'
		| 'priority_clear'
		| 'priority_p1'
		| 'priority_p2'
		| 'priority_p3'
		| 'priority_p4'
		| 'menu'
		| 'menu_delete'
		| 'edit_description'
		| 'schedule_edit'
		| 'doit_direct'
		| 'convert_monitor'
		| 'publish_notes'
		| 'view_result';
}

export interface ParsedTaskCompletionChange {
	taskId: string;
	checked: boolean;
}

export type TaskCreateSubmitValues = {
	task_title: string;
	task_description: string;
	task_output_mode: string;
	task_agent: string;
	task_thread: string;
	schedule_cron: string;
	schedule_timezone: string;
};
