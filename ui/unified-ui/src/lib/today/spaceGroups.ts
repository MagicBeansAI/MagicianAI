import type { TodayItem } from './types';

export type TodaySpaceGroup = {
	id: string;
	label: string;
	items: TodayItem[];
	unfiled: boolean;
};

export function formatSpaceLabel(spaceId: string): string {
	return spaceId
		.replace(/[_-]+/g, ' ')
		.trim()
		.split(/\s+/)
		.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
		.join(' ');
}

export function todaySpaceGroups(items: TodayItem[]): TodaySpaceGroup[] {
	const bySpace = new Map<string, TodaySpaceGroup>();
	for (const item of items) {
		const spaceId = item.space_ids.find((id) => id.trim().length > 0)?.trim() || 'unfiled';
		const existing =
			bySpace.get(spaceId) ??
			{
				id: spaceId,
				label: spaceId === 'unfiled' ? 'Other' : formatSpaceLabel(spaceId),
				items: [],
				unfiled: spaceId === 'unfiled'
			};
		existing.items.push(item);
		bySpace.set(spaceId, existing);
	}
	return [...bySpace.values()].sort((left, right) => {
		if (left.unfiled !== right.unfiled) return left.unfiled ? 1 : -1;
		return left.label.localeCompare(right.label);
	});
}
