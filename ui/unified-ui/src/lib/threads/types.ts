export type ThreadDisplayMode = 'chat' | 'dev';

export interface UiThreadRecord {
	principal: string;
	workspace: string;
	id: string;
	name: string;
	archived: boolean;
	sort_order: number;
	memory_summary?: string | null;
	memory_updated_at?: number | null;
	created_at: number;
	updated_at: number;
	history_lane?: 'personal' | 'automated';
	/** Developer Mode toggle — see docs/plans/2026-05-13-developer-mode-workbench.md. */
	display_mode?: ThreadDisplayMode;
	/** Plan-mode gate for Developer Mode (Phase 5). */
	plan_mode?: boolean;
}

export interface UiThreadDetail {
	principal: string;
	workspace: string;
	id: string;
	name: string;
	archived: boolean;
	sort_order: number;
	memory_summary?: string | null;
	memory_updated_at?: number | null;
	created_at: number;
	updated_at: number;
	history_lane?: 'personal' | 'automated';
	memory_text?: string | null;
	display_mode?: ThreadDisplayMode;
	plan_mode?: boolean;
}
