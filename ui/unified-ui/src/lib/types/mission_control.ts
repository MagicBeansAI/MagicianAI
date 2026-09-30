export interface ClarifierQuestionOption {
	value: string;
	label: string;
	description?: string;
}

export interface ClarifierQuestion {
	id: string;
	blocker_type: string;
	source_slot_id?: string | null;
	slot_confidence?: number | null;
	related_slots?: string[];
	question_text: string;
	context_snippets: string[];
	urgency: number;
	channel: string;
	created_at: string;
	options?: ClarifierQuestionOption[];
}

export type ClarificationStatus = 'waiting' | 'responding' | 'resuming' | 'resolved' | 'failed';

export interface ClarifierQuestionWithStatus extends ClarifierQuestion {
	status: ClarificationStatus;
	last_update?: number;
	note?: string;
}

export interface ClarificationSubmissionPayload {
	response_text: string;
}

export interface ClarificationResult {
	extracted_slots: SlotRecord[];
	workflow_resumed: boolean;
}

export interface ClarificationHistoryEntry {
	question_id: string;
	question_text: string;
	source_slot_id?: string;
	stage: StageContext;
	answered_at?: number;
	slot_updates: ClarificationHistorySlotUpdate[];
}

export interface ClarificationHistorySlotUpdate {
	slot_id: string;
	slot_type: string;
	value: unknown;
	confidence: number;
	received_at: number;
}

export interface StageContext {
	phase: string;
	attempt?: number;
	detail?: string;
}

export interface SlotRecord {
	id: string;
	slot_type: string;
	value: unknown;
	confidence: number;
	provenance: ProvenanceRecord[];
	evidence_links: string[];
	created_at: number;
	updated_at: number;
}

export interface ProvenanceRecord {
	source: string;
	timestamp: number;
}

export interface ManualResumeRequestPayload {
	slots?: ManualResumeSlotPayload[];
	updated_confidence?: number;
	handled_question?: string;
}

export interface ManualResumeSlotPayload {
	slot_id: string;
	slot_type: string;
	value: unknown;
	confidence?: number;
}

export interface ManualResumeResponse {
	workflow_resumed: boolean;
}

export interface ConfidenceSummary {
	overall: number;
	min_critical_slot: number | null;
	unresolved_slots: string[];
}

export interface EnrichmentSummary {
	total_slots: number;
	invocations: number;
	enrichments_applied: number;
	slots_changed: number;
	errors: EnrichmentError[];
}

export interface EnrichmentError {
	slot_id: string;
	enricher: string;
	message: string;
}

export type EnricherStatus = 'success' | 'error' | 'skipped';

export interface EnricherResult {
	name: string;
	status: EnricherStatus;
	slots_changed: number;
	error_message?: string;
}

export interface ClarifiedTask {
	clarified_task: string;
	constraints: string[];
	objectives: string[];
	resources: string[];
	confidence: number;
	open_questions: ClarifiedOpenQuestion[];
	slot_graph_id: string;
	original_message: string;
	rewrite_strategy?: string;
}

export interface ClarifiedOpenQuestion {
	question_text: string;
	context: string[];
	slot_id?: string;
	slot_confidence?: number;
	related_slots?: string[];
}

export interface SlotTriggerMapping {
	question_id: string;
	triggered_slots: TriggeringSlot[];
}

export interface TriggeringSlot {
	slot_id: string;
	slot_type: string;
	confidence: number;
	triggered_at: number;
}

export interface ConfidenceBoostResult {
	slot_id: string;
	old_confidence: number;
	new_confidence: number;
	boost_amount: number;
	resolved_at: number;
}
