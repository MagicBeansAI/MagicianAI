import migration from './legacySelections.v1.json';
import type { ModelSelection } from './modelCatalog';

/** Frozen conversion for returning clients; never an active routing alias map. */
export function migrateLegacyChatSelection(id: string | null | undefined): ModelSelection | null {
    return id && Object.hasOwn(migration.chat, id)
        ? (migration.chat as Record<string, ModelSelection>)[id] : null;
}

export function migrateLegacyCodingSelection(id: string | null): ({ kind: 'model'; engine: string } & ModelSelection) | null {
    return id && Object.hasOwn(migration.coding, id)
        ? (migration.coding as Record<string, { kind: 'model'; engine: string } & ModelSelection>)[id] : null;
}
