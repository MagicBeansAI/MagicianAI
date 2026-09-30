export interface CrewNativeComponent {
	id: string;
	component_type: string;
	label?: string;
	source?: string;
	query?: string;
	props: Record<string, unknown>;
	static_snapshot?: unknown;
	children?: CrewNativeComponent[];
}

export type CrewNativeInteractionKind =
	| 'submit'
	| 'search'
	| 'change'
	| 'action'
	| 'confirm'
	| 'cancel'
	| 'dismiss'
	| 'close';

export interface CrewNativeInteractionEventDetail {
	componentId: string;
	interaction: CrewNativeInteractionKind;
	detail: Record<string, unknown>;
	sent: boolean;
}
