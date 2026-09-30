import type { StructuredResponseActionV1 } from './types';

const KIND_LABEL_BY_KIND: Record<string, string> = {
	copy_text: 'Copy text',
	open_url: 'Open URL',
	open_task: 'Open task',
	open_artifact: 'Open artifact',
	send_follow_up: 'Send follow-up',
	invoke_server_action: 'Invoke server action'
};

export interface StructuredResponseActionPresentation {
	label: string;
	description: string;
	action: StructuredResponseActionV1;
}

export function presentationForAction(action: StructuredResponseActionV1): StructuredResponseActionPresentation {
	return {
		label: action.label,
		description: KIND_LABEL_BY_KIND[action.kind] ?? 'Action',
		action
	};
}

export function isActionRenderable(_action: StructuredResponseActionV1): boolean {
	return true;
}
