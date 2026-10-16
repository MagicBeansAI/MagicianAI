import type { MuijDocument } from '$lib/stores/muijStore';

export const SURFACE_MANIFEST_ARTIFACT_NAME = 'surface_manifest' as const;
export const SURFACE_MANIFEST_ARTIFACT_TYPE = 'custom:surface_manifest' as const;
export const PUBLISHED_SURFACE_CHANGED_EVENT_TYPE = 'published_surface.changed' as const;
export const SURFACE_SCHEMA_VERSION = '1.0' as const;

export interface SurfaceMuijRef {
	document_key: string;
	root_component_id?: string;
}

export interface SurfaceFreshness {
	expires_at?: string;
	ttl_seconds?: number;
}

export interface SurfaceSpatial {
	x: number;
	y: number;
	width: number;
	height: number;
	layer?: string;
}

export interface SurfaceManifest {
	surface_version: string;
	surface_id: string;
	route: string;
	title: string;
	summary?: string;
	input_artifacts: string[];
	tags: string[];
	search_text?: string;
	spatial?: SurfaceSpatial;
	muij_ref: SurfaceMuijRef;
	published_at: string;
	freshness?: SurfaceFreshness;
}

export interface SurfaceOwnershipScope {
	execution_id?: string;
	task_id?: string;
	workflow_instance_id?: string;
	run_id?: string;
	cycle_id?: string;
}

export interface SurfaceProducerInfo {
	producer_agent_id: string;
	producer_stage?: string;
	produced_at: string;
}

export type SurfacePhysicalLocator =
	| { type: 'pipeline_store'; chain_id: string; artifact_id: string }
	| { type: 'workflow_run'; instance_id: string; step_name: string; artifact_name: string }
	| { type: 'episode_file'; agent_id: string; episode_id: string }
	| { type: 'in_memory'; key: string }
	| { type: 'durable_store'; namespace: string; name: string };

export interface SurfaceArtifactMetadata {
	artifact_uid: string;
	domain: string;
	artifact_type?: string;
	physical_locator: SurfacePhysicalLocator;
	route_target?: string;
	ownership: SurfaceOwnershipScope;
	producer: SurfaceProducerInfo;
	lifecycle_state: string;
}

export interface SurfaceManifestQuery {
	domain?: string;
	artifact_type?: string;
	route_target?: string;
	execution_id?: string;
	task_id?: string;
	agent_id?: string;
	workflow_instance_id?: string;
	run_id?: string;
	cycle_id?: string;
	limit?: number;
	offset?: number;
}

export interface PublishedSurfaceRecord {
	metadata: SurfaceArtifactMetadata;
	manifest: SurfaceManifest;
	manifest_name: string;
	task_title?: string;
	task_status?: string;
	source_agent_id?: string;
	source_output_media_type?: string;
	source_output_summary?: string;
	render_origin?: string;
	render_kind?: string;
	presentation_state?: string;
	render?: PublishedSurfaceRenderRecord;
}

export interface PublishedSurfaceRenderRecord {
	surface: {
		surface_id: string;
		task_id?: string;
		ui_thread_id?: string;
		source_output_id?: string;
		source_execution_id?: string;
		media_type?: string;
		title: string;
		summary?: string;
		route: string;
		document_key: string;
		status: string;
		manifest_name?: string;
	};
	task_title?: string;
	task_status?: string;
	source_agent_id?: string;
	source_output_id?: string;
	source_execution_id?: string;
	source_output_relative_path?: string;
	source_output_summary?: string;
	media_type?: string;
	render_origin: string;
	render_kind: string;
	presentation_state: string;
	durable_document_key?: string;
	durable_manifest_name?: string;
	muij_document?: MuijDocument;
	text_content?: string;
	json_content?: unknown;
	content_truncated?: boolean;
	unavailable_reason?: string;
}

export interface PublishedSurfaceRefreshRealtimeEvent {
	event_type: typeof PUBLISHED_SURFACE_CHANGED_EVENT_TYPE;
	producer_agent_id: string;
	surface_id: string;
	principal: string;
	workspace: string;
	route: string;
	document_key: string;
	status: string;
	surface_kind?: string;
	logical_surface_id?: string;
	task_id?: string;
	ui_thread_id?: string;
	source_output_id?: string;
	execution_id?: string;
	published_at?: string;
	updated_at?: string;
}
