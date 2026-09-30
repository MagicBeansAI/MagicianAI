/**
 * Shared XML viewer types — kept in a plain TS module so both XmlNode.svelte
 * (recursive renderer) and XmlViewer.svelte (root container) can import them.
 * Putting `export interface` inside a `.svelte` component script doesn't work
 * for cross-component imports.
 */

export interface XmlNodeInfo {
	kind: 'element' | 'text' | 'cdata' | 'comment' | 'pi';
	name?: string;
	attributes?: Array<{ name: string; value: string }>;
	children?: XmlNodeInfo[];
	text?: string;
}
