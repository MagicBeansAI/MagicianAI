import type { ChatMessageContent, ContentBlockRecord } from '$lib/stores/chatStore';
import { getEscalationResolvedSummary } from '$lib/stores/chatStore';
import type {
	StructuredResponseActionV1,
	StructuredArtifactRefV1,
	StructuredResponseBlockV1,
	StructuredCalloutBlockV1,
	StructuredResponseMetaV1,
	StructuredProvenanceRefV1,
	StructuredResponseV1,
	StructuredSourceRefV1
} from './types';
import { toPlainText } from './toPlainText';

type FileContentBlockRecord = ContentBlockRecord & { type: 'file' };
type TextContentBlockRecord = ContentBlockRecord & { type: 'text' };
type UrlContentBlockRecord = ContentBlockRecord & { type: 'url'; url: string };

const MAX_ARTIFACT_SIZE = 2_147_483_647;
const STRUCTURED_ARTIFACT_SESSION_PREFIX = 'magician-artifact:session:';
const STRUCTURED_ARTIFACT_TASK_PREFIX = 'magician-artifact:task:';

export interface StructuredContentAdaptation {
	supported: boolean;
	response: StructuredResponseV1 | null;
	reason?: string;
	plain_text: string;
}

function normalizedText(value: unknown): string {
	if (typeof value !== 'string') return '';
	return value.trim();
}

function appendMarkdownBlock(blocks: StructuredResponseBlockV1[], text: string): void {
	if (!text) return;
	blocks.push({ kind: 'markdown', text });
}

function boundedArtifactSize(value: unknown): number | undefined {
	return typeof value === 'number' &&
		Number.isSafeInteger(value) &&
		value >= 0 &&
		value <= MAX_ARTIFACT_SIZE
		? value
		: undefined;
}

function isUrlContentBlock(block: ContentBlockRecord): block is UrlContentBlockRecord {
	return block.type === 'url' && typeof block.url === 'string' && block.url.trim().length > 0;
}

function logicalArtifactId(block: FileContentBlockRecord, taskId?: string): string | undefined {
	const relativePath = normalizedText(block.relative_path);
	if (!relativePath) return undefined;
	const normalizedTaskId = normalizedText(taskId);
	return normalizedTaskId
		? `${STRUCTURED_ARTIFACT_TASK_PREFIX}${normalizedTaskId}:${relativePath}`
		: `${STRUCTURED_ARTIFACT_SESSION_PREFIX}${relativePath}`;
}

function toArtifactRefsFromBlocks(
	blocks: ContentBlockRecord[],
	taskId?: string
): StructuredArtifactRefV1[] {
	return blocks
		.filter((block): block is FileContentBlockRecord => block.type === 'file')
		.map((block) => ({
			label: block.display_name?.trim() || block.relative_path?.trim() || 'attachment',
			mime_type: block.mime_type?.trim() || undefined,
			size: boundedArtifactSize(block.size),
			artifact_id: logicalArtifactId(block, taskId)
		}));
}

function toProvenanceFromBlocks(blocks: ContentBlockRecord[]): StructuredProvenanceRefV1[] {
	return blocks
		.filter(isUrlContentBlock)
		.map((block) => ({
			id: block.url,
			label: block.label?.trim() || block.url,
			ref: block.url
		}));
}

function toSourceRefsFromBlocks(blocks: ContentBlockRecord[]): StructuredSourceRefV1[] {
	return blocks
		.filter(isUrlContentBlock)
		.map((block) => ({
			label: block.label?.trim() || block.url,
			href: block.url
		}));
}

function taskStatusTone(status: string | undefined): StructuredResponseV1['tone'] {
	switch (status) {
		case 'completed':
			return 'success';
		case 'failed':
		case 'cancelled':
		case 'expired':
			return 'danger';
		case 'warning':
			return 'warning';
		case 'info':
			return 'info';
		default:
			return 'neutral';
	}
}

function taskStatusCalloutTone(status: string | undefined): StructuredCalloutBlockV1['tone'] {
	const tone = taskStatusTone(status);
	return tone === 'neutral' || tone === undefined ? 'info' : tone;
}

function attachFinalPlainText(
	response: Omit<StructuredResponseV1, 'plain_text'>,
	overridePlainText?: string
): StructuredResponseV1 {
	const draft: StructuredResponseV1 = {
		schema: response.schema,
		version: response.version,
		plain_text: '',
		title: response.title,
		summary: response.summary,
		tone: response.tone,
		blocks: response.blocks,
		actions: response.actions,
		model_context: response.model_context,
		meta: response.meta
	};

	return {
		...draft,
		plain_text: normalizedText(overridePlainText) || toPlainText(draft)
	};
}

export function adaptLegacyChatContentToStructuredResponse(
	content: ChatMessageContent
): StructuredContentAdaptation {
	try {
		switch (content.type) {
			case 'text': {
				const text = normalizedText(content.text);
				const response = attachFinalPlainText({
					schema: 'magician.structured_response',
					version: 1,
					blocks: [{ kind: 'markdown', text }]
				});
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'tool_call_executed': {
				const toolName = normalizedText(content.tool_name);
				const summary = normalizedText(content.summary);
				const title = toolName ? `Action completed: ${toolName}` : 'Action completed';
				const response = attachFinalPlainText({
					schema: 'magician.structured_response',
					version: 1,
					title,
					summary,
					tone: 'info',
					blocks: [{ kind: 'callout', tone: 'info', title, text: summary || 'Action completed.' }]
				});
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'rich_tool_result': {
				const summary = normalizedText(content.summary);
				const blocks: StructuredResponseBlockV1[] = [];
				appendMarkdownBlock(blocks, summary);
				const artifactItems = toArtifactRefsFromBlocks(content.content_blocks ?? []);
				const provenanceEntries = toProvenanceFromBlocks(content.content_blocks ?? []);
				const textBlocks = (content.content_blocks ?? [])
					.filter(
						(block): block is TextContentBlockRecord => block.type === 'text'
					)
					.map((block) => normalizedText(block.text));
				for (const value of textBlocks) {
					appendMarkdownBlock(blocks, value);
				}
				if (artifactItems.length > 0) {
					blocks.push({ kind: 'artifacts', items: artifactItems, title: 'Outputs' });
				}

				const response = attachFinalPlainText({
					schema: 'magician.structured_response',
					version: 1,
					title: 'Action result',
					blocks,
					meta: provenanceEntries.length > 0 ? { provenance: provenanceEntries } : undefined
				});
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'attachment': {
				const filename = normalizedText(content.filename);
				const label = filename || normalizedText(content.label) || 'Attachment';
				const artifact: StructuredArtifactRefV1 = {
					label,
					artifact_id: filename ? `${STRUCTURED_ARTIFACT_SESSION_PREFIX}${filename}` : undefined,
					mime_type: normalizedText(content.mime_type) || undefined
				};
				const response = attachFinalPlainText(
					{
						schema: 'magician.structured_response',
						version: 1,
						title: 'Attachment',
						blocks: [{ kind: 'artifacts', items: [artifact] }]
					},
					`Attachment: ${label}`
				);
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'task_status_update': {
				const taskId = normalizedText(content.task_id);
				const status = normalizedText(content.status);
				const summary = normalizedText(content.summary);
				const spokenSummary = normalizedText(content.speech_tts);
				const statusTitle = taskId ? `Task ${taskId}: ${status || 'status update'}` : 'Task status';
				const blocks: StructuredResponseBlockV1[] = [{
					kind: 'callout',
					tone: taskStatusCalloutTone(status),
					text: summary || spokenSummary || `${statusTitle} summary`
				}];

				const actions: StructuredResponseActionV1[] = [];
				if (taskId) {
					actions.push({
						kind: 'open_task',
						label: 'Open task',
						task_id: taskId
					});
				}

				const outputFiles = content.output_files ?? [];
				if (outputFiles.length > 0) {
					blocks.push({
						kind: 'artifacts',
						title: 'Outputs',
						items: toArtifactRefsFromBlocks(outputFiles, taskId)
					});
				}

				const metadata: StructuredResponseMetaV1 = {};
				if (taskId) {
					metadata.task_id = taskId;
				}
				const executionId = normalizedText(content.execution_id);
				if (executionId) {
					metadata.execution_id = executionId;
				}
				if (content.ui_thread_id) {
					metadata.chat_turn_id = content.ui_thread_id;
				}

				const response = attachFinalPlainText({
					schema: 'magician.structured_response',
					version: 1,
					title: statusTitle,
					tone: taskStatusTone(status),
					summary: summary || spokenSummary || `${status || 'completed'} status`,
					blocks,
					actions,
					meta: Object.keys(metadata).length > 0 ? metadata : undefined
				});
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'escalation_resolved': {
				const summary = getEscalationResolvedSummary(content);
				const outputFiles = content.output_files ?? [];
				const outputFileRefs = toArtifactRefsFromBlocks(outputFiles, content.task_id);
				const outputSources = toSourceRefsFromBlocks(outputFiles);
				const outputTextBlocks = outputFiles
					.filter((block): block is TextContentBlockRecord => block.type === 'text')
					.map((block) => normalizedText(block.text));

				const blocks: StructuredResponseBlockV1[] = [{
					kind: 'callout',
					tone: 'success',
					title: 'Escalation resolved',
					text: summary || 'Escalation resolved.'
				}];
				if (outputFileRefs.length > 0) {
					blocks.push({ kind: 'artifacts', title: 'Output files', items: outputFileRefs });
				}
				for (const value of outputTextBlocks) {
					appendMarkdownBlock(blocks, value);
				}
				if (outputSources.length > 0) {
					blocks.push({ kind: 'sources', title: 'Output links', items: outputSources });
				}
				const response = attachFinalPlainText({
					schema: 'magician.structured_response',
					version: 1,
					title: 'Escalation resolved',
					tone: 'success',
					summary,
					blocks,
					meta: content.task_id ? { task_id: content.task_id } : undefined
				});
				return {
					supported: true,
					plain_text: response.plain_text,
					response
				};
			}
			case 'escalation':
				return {
					supported: false,
					reason: 'active_escalation_requires_specialized_ui',
					plain_text: `Action Required: ${normalizedText(content.question) || 'Respond to user request'}`,
					response: null
				};
			default:
				return {
					supported: false,
					reason: `unsupported_content_type:${content.type}`,
					plain_text: normalizedText(`Unsupported chat content type: ${content.type}`),
					response: null
				};
		}
	} catch (error) {
		const fallback = normalizedText(
			typeof content === 'object' && 'type' in content ? `${content.type}_render_fallback` : 'Unsupported chat content'
		);
		return {
			supported: false,
			reason: error instanceof Error ? error.message : 'adaptation_failed',
			plain_text: fallback,
			response: null
		};
	}
}
