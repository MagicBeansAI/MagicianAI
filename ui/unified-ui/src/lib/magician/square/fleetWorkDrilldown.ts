import type { CitizenCurrentWorkVM } from './engine/types';
import type { Quest, QuestState } from './fleetQuests';

const MAX_ADDITIONAL_CURRENT = 2;
const MAX_ARTIFACTS = 4;

export interface WorkDrilldownSubject {
	id: string;
	currentWork: CitizenCurrentWorkVM[];
	lastOutcome?: string;
}

export interface WorkDrilldownItem {
	taskId: string | null;
	title: string;
	objective: string;
	state: QuestState | 'unknown';
	status: string;
	currentStep: string | null;
	blocker: string | null;
	outcome: string | null;
	artifacts: string[];
	updatedAt: number | null;
}

export interface WorkDrilldownModel {
	focus: WorkDrilldownItem | null;
	additionalCurrent: WorkDrilldownItem[];
	latestOutcome: WorkDrilldownItem | null;
	activeCount: number;
}

function byNewest(left: Quest, right: Quest): number {
	return (right.updatedAt ?? right.createdAt ?? 0) - (left.updatedAt ?? left.createdAt ?? 0);
}

function isCurrent(quest: Quest): boolean {
	return ['planning', 'active', 'awaiting_orders', 'blocked', 'delivering'].includes(quest.state);
}

function isOutcome(quest: Quest): boolean {
	return ['succeeded', 'failed', 'cancelled'].includes(quest.state);
}

function blockerOf(quest: Quest): string | null {
	const question = quest.pendingQuestions[0]?.prompt?.trim();
	if (question) return question;
	if (quest.isBlocked || quest.state === 'blocked') return 'Waiting on a prerequisite or decision.';
	if (quest.state === 'awaiting_orders') return 'Waiting for operator direction.';
	return null;
}

function outcomeOf(quest: Quest): string | null {
	return quest.completionSummary?.trim()
		|| quest.completionOutcome?.trim()
		|| (quest.state === 'failed' ? 'The task ended without a successful result.' : null);
}

function currentStepOf(step: string | null, substep: string | null): string | null {
	const parts = [step?.trim(), substep?.trim()].filter(
		(value): value is string => Boolean(value)
	);
	return Array.from(new Set(parts)).join(' · ') || null;
}

function fromQuest(quest: Quest): WorkDrilldownItem {
	return {
		taskId: quest.id,
		title: quest.title,
		objective: quest.description.trim() || quest.title,
		state: quest.state,
		status: quest.status,
		currentStep: currentStepOf(quest.currentStep, quest.currentSubstep),
		blocker: blockerOf(quest),
		outcome: outcomeOf(quest),
		artifacts: quest.artifacts.slice(0, MAX_ARTIFACTS),
		updatedAt: quest.updatedAt ?? quest.createdAt
	};
}

function fromCurrentWork(work: CitizenCurrentWorkVM): WorkDrilldownItem {
	return {
		taskId: work.questId,
		title: work.title,
		objective: work.title,
		state: work.isBlocked ? 'blocked' : 'unknown',
		status: work.status,
		currentStep: currentStepOf(work.currentStep, work.currentSubstep),
		blocker: work.isBlocked ? 'Waiting on a prerequisite or decision.' : null,
		outcome: null,
		artifacts: [],
		updatedAt: Date.parse(work.updatedAt) || null
	};
}

function fallbackOutcome(summary: string): WorkDrilldownItem {
	return {
		taskId: null,
		title: 'Latest outcome',
		objective: summary,
		state: 'succeeded',
		status: 'completed',
		currentStep: null,
		blocker: null,
		outcome: summary,
		artifacts: [],
		updatedAt: null
	};
}

export function buildCitizenWorkDrilldown(
	subject: WorkDrilldownSubject,
	quests: Quest[]
): WorkDrilldownModel {
	const questById = new Map(quests.map((quest) => [quest.id, quest]));
	const assigned = quests.filter((quest) => quest.agentId === subject.id);
	const orderedCurrent: WorkDrilldownItem[] = [];
	const seen = new Set<string>();

	for (const work of subject.currentWork) {
		if (seen.has(work.questId)) continue;
		seen.add(work.questId);
		orderedCurrent.push(questById.has(work.questId) ? fromQuest(questById.get(work.questId)!) : fromCurrentWork(work));
	}
	for (const quest of assigned.filter(isCurrent).sort(byNewest)) {
		if (seen.has(quest.id)) continue;
		seen.add(quest.id);
		orderedCurrent.push(fromQuest(quest));
	}

	const latestQuestOutcome = assigned.filter(isOutcome).sort(byNewest)[0];
	const outcome = latestQuestOutcome
		? fromQuest(latestQuestOutcome)
		: subject.lastOutcome?.trim()
			? fallbackOutcome(subject.lastOutcome.trim())
			: null;

	return {
		focus: orderedCurrent[0] ?? null,
		additionalCurrent: orderedCurrent.slice(1, 1 + MAX_ADDITIONAL_CURRENT),
		latestOutcome: outcome,
		activeCount: orderedCurrent.length
	};
}

export function buildGuildWorkDrilldown(memberIds: string[], quests: Quest[]): WorkDrilldownModel {
	const members = new Set(memberIds);
	const assigned = quests.filter((quest) => quest.agentId && members.has(quest.agentId));
	const current = assigned.filter(isCurrent).sort(byNewest).map(fromQuest);
	const latestQuestOutcome = assigned.filter(isOutcome).sort(byNewest)[0];

	return {
		focus: current[0] ?? null,
		additionalCurrent: current.slice(1, 1 + MAX_ADDITIONAL_CURRENT),
		latestOutcome: latestQuestOutcome ? fromQuest(latestQuestOutcome) : null,
		activeCount: current.length
	};
}
