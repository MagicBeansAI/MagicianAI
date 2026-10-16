import type { Quest, QuestState } from './fleetQuests';

export type ObjectiveUrgency = 'critical' | 'attention' | 'active' | 'steady' | 'complete';

export interface FleetObjective {
	quest: Quest;
	eyebrow: string;
	title: string;
	detail: string;
	progressLabel: string;
	urgency: ObjectiveUrgency;
	actionLabel: string;
}

const PRIORITY_WEIGHT: Record<string, number> = {
	critical: 40,
	urgent: 40,
	high: 25,
	medium: 10,
	low: 0
};

const STATE_WEIGHT: Record<QuestState, number> = {
	awaiting_orders: 100,
	blocked: 90,
	delivering: 70,
	active: 60,
	planning: 50,
	ready: 40,
	failed: 20,
	succeeded: 0,
	cancelled: -10
};

export function questStateLabel(state: QuestState): string {
	switch (state) {
		case 'awaiting_orders':
			return 'Needs you';
		case 'blocked':
			return 'Blocked';
		case 'delivering':
			return 'Preparing delivery';
		case 'active':
			return 'In progress';
		case 'planning':
			return 'Planning';
		case 'ready':
			return 'Ready to start';
		case 'succeeded':
			return 'Delivered';
		case 'failed':
			return 'Failed';
		case 'cancelled':
			return 'Cancelled';
	}
}

function scoreQuest(quest: Quest): number {
	const priority = PRIORITY_WEIGHT[quest.priority?.toLowerCase() ?? ''] ?? 0;
	const due = quest.dueDate ? Date.parse(quest.dueDate) : Number.NaN;
	const dueWeight = Number.isFinite(due) && due < Date.now() + 86_400_000 ? 20 : 0;
	return STATE_WEIGHT[quest.state] + priority + dueWeight + (quest.updatedAt ?? 0) / 1e15;
}

export function chooseFleetObjective(quests: Quest[]): FleetObjective | null {
	const candidates = quests.filter(
		(quest) => quest.state !== 'cancelled' && quest.state !== 'succeeded'
	);
	if (candidates.length === 0) {
		const latestDelivery = quests
			.filter((quest) => quest.state === 'succeeded')
			.sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0))[0];
		return latestDelivery ? toObjective(latestDelivery) : null;
	}
	return toObjective([...candidates].sort((a, b) => scoreQuest(b) - scoreQuest(a))[0]);
}

function toObjective(quest: Quest): FleetObjective {
	const currentWork = quest.currentSubstep ?? quest.currentStep;
	const question = quest.pendingQuestions[0]?.prompt;
	const fallbackDetail = quest.description.trim() || 'No task details';
	const detail =
		question ??
		currentWork ??
		quest.completionSummary ??
		fallbackDetail;
	const urgency: ObjectiveUrgency =
		quest.state === 'awaiting_orders' || quest.state === 'failed'
			? 'critical'
			: quest.state === 'blocked'
				? 'attention'
				: quest.state === 'active' || quest.state === 'delivering'
					? 'active'
					: quest.state === 'succeeded'
						? 'complete'
						: 'steady';
	const actionLabel =
		quest.state === 'awaiting_orders'
			? 'Review decision'
			: quest.state === 'blocked' || quest.state === 'failed'
				? 'Inspect obstacle'
				: quest.state === 'succeeded'
					? 'Review delivery'
					: 'Open task';

	return {
		quest,
		eyebrow: quest.priority ? `${quest.priority} priority` : 'Task',
		title: quest.title,
		detail,
		progressLabel: questStateLabel(quest.state),
		urgency,
		actionLabel
	};
}

export interface QuestDependencyNode {
	quest: Quest;
	prerequisites: Quest[];
	dependents: Quest[];
}

export function buildQuestDependencyGraph(quests: Quest[]): QuestDependencyNode[] {
	const byId = new Map(quests.map((quest) => [quest.id, quest]));
	const dependents = new Map<string, Quest[]>();
	for (const quest of quests) {
		for (const prerequisiteId of quest.dependsOn) {
			const list = dependents.get(prerequisiteId) ?? [];
			list.push(quest);
			dependents.set(prerequisiteId, list);
		}
	}
	return quests.map((quest) => ({
		quest,
		prerequisites: quest.dependsOn
			.map((id) => byId.get(id))
			.filter((item): item is Quest => item != null),
		dependents: dependents.get(quest.id) ?? []
	}));
}
