import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const chatPanel = readFileSync(join(process.cwd(), 'src/lib/magician/chat/ChatPanel.svelte'), 'utf8');
const schema = readFileSync(
	join(process.cwd(), 'src/lib/magician/structuredResponse/schema.ts'),
	'utf8'
);
const speakButton = readFileSync(
	join(process.cwd(), 'src/lib/magician/components/chat/SpeakButton.svelte'),
	'utf8'
);

describe('ChatPanel structured response migration', () => {
	it('renders generic message kinds from validated server presentations only', () => {
		expect(chatPanel).toContain("resolveStructuredResponseForMessage(message, 'tool_call_executed')");
		expect(chatPanel).toContain("resolveStructuredResponseForMessage(message, 'rich_tool_result')");
		expect(chatPanel).toContain("resolveStructuredResponseForMessage(message, 'escalation_resolved')");
		expect(chatPanel).toContain("resolveStructuredResponseForMessage(message, 'attachment')");
		expect(chatPanel).toContain('return resolveStructuredResponseFromMessagePresentation(message, kind);');
		expect(chatPanel).not.toContain('adaptLegacyChatContentToStructuredResponse');
		expect(chatPanel).toContain('validateStructuredResponse(presentation)');
	});

	it('retains specialized authoritative HITL and live-task components', () => {
		expect(chatPanel).toContain("{:else if message.content.type === 'escalation'}");
		expect(chatPanel).toContain('EscalationCard');
		expect(chatPanel).not.toContain("resolveStructuredResponseForMessage(message, 'escalation')");
		expect(chatPanel).toContain('resolveStructuredResponseForTaskStatusUpdate(message)');
		expect(chatPanel).toContain('<TaskStatusCard');
	});

	it('rejects deprecated mutating server action records', () => {
		expect(schema).toContain('action[${index}].kind is unsupported');
	});

	it('keeps structured task-output artifact URLs scope-bound', () => {
		expect(chatPanel).toContain('taskOutputUrl(taskId, relativePath, scope.principal, scope.workspace)');
	});

	it('keeps conversation-wide build out of repeated message footers', () => {
		expect(chatPanel).not.toContain('chat-bubble-actions');
		expect(chatPanel).not.toContain('chat-build-in-vibe');
		expect(chatPanel).toContain('canBuild={canBuildInVibe}');
		expect(chatPanel).toContain('on:build={buildInVibe}');
	});

	it('houses message playback in the metadata header', () => {
		expect(chatPanel).toMatch(
			/<div class="chat-header">[\s\S]*?<span class="chat-header-speak">[\s\S]*?<SpeakButton/
		);
		expect(chatPanel).toContain('text={spokenText}');
	});

	it('keeps message playback readable on its host surface in every interaction state', () => {
		expect(speakButton).toContain('color: inherit;');
		expect(speakButton).toContain('border-color: currentColor;');
		expect(speakButton).toContain('color-mix(in srgb, currentColor 12%, transparent)');
		expect(speakButton).not.toContain('color: var(--theme-color-foreground');
		expect(speakButton).not.toContain('color: var(--theme-color-accent');
	});
});
