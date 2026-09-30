export interface ChannelFollowUpVisibilityState {
	loading: boolean;
	error: string | null;
	total: number;
	itemCount: number;
}

export interface ChannelFollowUpPagination {
	pageCount: number;
	startItem: number;
	endItem: number;
}

export function shouldRenderChannelFollowUps({
	loading,
	error,
	total,
	itemCount
}: ChannelFollowUpVisibilityState): boolean {
	return loading || error !== null || total > 0 || itemCount > 0;
}

export function channelFollowUpPagination(
	total: number,
	page: number,
	itemCount: number,
	pageSize: number
): ChannelFollowUpPagination {
	const safeTotal = Math.max(0, Math.floor(total));
	const safePage = Math.max(1, Math.floor(page));
	const safePageSize = Math.max(1, Math.floor(pageSize));
	const safeItemCount = Math.max(0, Math.floor(itemCount));
	return {
		pageCount: Math.max(1, Math.ceil(safeTotal / safePageSize)),
		startItem: safeTotal === 0 ? 0 : (safePage - 1) * safePageSize + 1,
		endItem:
			safeTotal === 0
				? 0
				: Math.min(safeTotal, (safePage - 1) * safePageSize + safeItemCount)
	};
}
