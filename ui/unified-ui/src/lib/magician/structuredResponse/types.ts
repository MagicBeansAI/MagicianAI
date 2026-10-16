export interface StructuredResponseV1 {
	schema: 'magician.structured_response';
	version: 1;
	plain_text: string;
	title?: string;
	summary?: string;
	tone?: 'neutral' | 'success' | 'warning' | 'danger' | 'info';
	blocks: StructuredResponseBlockV1[];
	actions?: StructuredResponseActionV1[];
	model_context?: StructuredModelContextV1;
	meta?: StructuredResponseMetaV1;
}

export type StructuredResponseBlockV1 =
	| StructuredTextBlockV1
	| StructuredMarkdownBlockV1
	| StructuredCalloutBlockV1
	| StructuredKeyValuesBlockV1
	| StructuredTableBlockV1
	| StructuredListBlockV1
	| StructuredArtifactsBlockV1
	| StructuredSourcesBlockV1
	| StructuredMetricsBlockV1;

export interface StructuredResponseBlockBase {
	kind: string;
	title?: string;
}

export interface StructuredTextBlockV1 extends StructuredResponseBlockBase {
	kind: 'text';
	text: string;
}

export interface StructuredMarkdownBlockV1 extends StructuredResponseBlockBase {
	kind: 'markdown';
	text: string;
}

export interface StructuredCalloutBlockV1 extends StructuredResponseBlockBase {
	kind: 'callout';
	tone: 'info' | 'success' | 'warning' | 'danger';
	text: string;
}

export interface StructuredKeyValuesBlockV1 extends StructuredResponseBlockBase {
	kind: 'key_values';
	items: StructuredKeyValueV1[];
}

export interface StructuredKeyValueV1 {
	label: string;
	value: string;
	hint?: string;
}

export interface StructuredTableColumn {
	key: string;
	label: string;
	alignment?: 'start' | 'center' | 'end';
}

export interface StructuredTableBlockV1 extends StructuredResponseBlockBase {
	kind: 'table';
	columns: StructuredTableColumn[];
	rows: Array<Record<string, string>>;
}

export interface StructuredListBlockV1 extends StructuredResponseBlockBase {
	kind: 'list';
	style?: 'bullets' | 'steps' | 'checks';
	items: StructuredListItemV1[];
}

export interface StructuredListItemV1 {
	text: string;
	detail?: string;
	checked?: boolean;
}

export interface StructuredArtifactsBlockV1 extends StructuredResponseBlockBase {
	kind: 'artifacts';
	items: StructuredArtifactRefV1[];
}

export interface StructuredArtifactRefV1 {
	label: string;
	href?: string;
	artifact_id?: string;
	mime_type?: string;
	size?: number;
}

export interface StructuredSourcesBlockV1 extends StructuredResponseBlockBase {
	kind: 'sources';
	items: StructuredSourceRefV1[];
}

export interface StructuredSourceRefV1 {
	label: string;
	href: string;
}

export interface StructuredMetricV1 {
	label: string;
	value: string;
	trend?: 'up' | 'down' | 'flat';
	unit?: string;
}

export interface StructuredMetricsBlockV1 extends StructuredResponseBlockBase {
	kind: 'metrics';
	items: StructuredMetricV1[];
}

export type StructuredResponseActionV1 =
	| StructuredCopyTextActionV1
	| StructuredOpenUrlActionV1
	| StructuredOpenTaskActionV1
	| StructuredOpenArtifactActionV1
	| StructuredSendFollowUpActionV1
	| StructuredInvokeServerActionV1;

export interface StructuredCopyTextActionV1 {
	kind: 'copy_text';
	label: string;
	text: string;
}

export interface StructuredOpenUrlActionV1 {
	kind: 'open_url';
	label: string;
	url: string;
}

export interface StructuredOpenTaskActionV1 {
	kind: 'open_task';
	label: string;
	task_id: string;
}

export interface StructuredOpenArtifactActionV1 {
	kind: 'open_artifact';
	label: string;
	artifact_id: string;
}

export interface StructuredSendFollowUpActionV1 {
	kind: 'send_follow_up';
	label: string;
	prompt: string;
}

export interface StructuredInvokeServerActionV1 {
	kind: 'invoke_server_action';
	label: string;
	action_ref: string;
}

export interface StructuredResponseMetaV1 {
	response_id?: string;
	source_surface?: string;
	task_id?: string;
	execution_id?: string;
	chat_turn_id?: string;
	provenance?: StructuredProvenanceRefV1[];
	cost?: {
		input_tokens?: number;
		output_tokens?: number;
		total_tokens?: number;
		cost_usd?: number;
		model?: string;
	};
	confidence?: number;
	created_at?: string;
}

export interface StructuredProvenanceRefV1 {
	id: string;
	label?: string;
	ref?: string;
}

export interface StructuredModelContextV1 {
	summary: string;
	visible_facts?: string[];
	selected_item?: string;
	privacy: 'model_visible' | 'local_only';
}
