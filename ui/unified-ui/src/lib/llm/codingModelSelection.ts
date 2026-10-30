import type { ModelChoice } from './modelCatalog';
import type { CodingProfile, SavedCodingSelection } from '$lib/stores/codingProfileStore';
import { timedFetch } from '$lib/shared/fetch';

export interface CodingEngineStatus {
    id: string;
    selectable: boolean;
    readiness?: string;
    reason?: string | null;
    gateway?: CodingProfile['gateway'];
    /** Whose credentials a launch uses: `login`, `magician_key` or `none`. */
    credentials?: string;
}

export interface CodingModelChoice extends ModelChoice { engine: string }

export interface CodingModelCatalog {
    models: CodingModelChoice[];
    engines: CodingEngineStatus[];
    default_selection: SavedCodingSelection | null;
}

/** Shared composer/crew reader for the model catalog. */
export async function fetchCodingModelRows(): Promise<{ rows: CodingProfile[] }> {
    const response = await timedFetch('/api/magician/v2/coding/models');
    if (!response.ok) throw new Error(`Coding model choices could not be loaded (HTTP ${response.status}).`);
    const body = await response.json();
    return { rows: codingModelRows(body) };
}

/** Picker rows are disposable projections; preferences and requests stay typed. */
export function codingModelRows(catalog: CodingModelCatalog): CodingProfile[] {
    const initial = catalog.default_selection;
    const models = catalog.models.flatMap(model => {
        const engine = catalog.engines.find(engine => engine.id === model.engine);
        if (!engine?.selectable) return [];
        return Object.entries(model.choices).map(([thinking, id]) => ({
            id, label: model.label, engine: model.engine, provider: model.provider, model: model.model,
            selectable: true, readiness: engine.readiness, gateway: engine.gateway, credentials: engine.credentials,
            supports_user_image_inputs: model.capabilities.vision,
            is_default: initial?.kind === 'model' && initial.engine === model.engine
                && initial.model_id === model.id && initial.thinking === thinking,
            model_selection: { model_id: model.id, thinking },
            model_choice: thinking === 'auto' ? model : null
        }));
    });
    return [...models, ...catalog.engines.filter(engine => !engine.selectable).map(engine => ({
        id: `unavailable/${engine.id}`, label: engine.id, engine: engine.id,
        selectable: false, readiness: engine.readiness, reason: engine.reason, gateway: engine.gateway, credentials: engine.credentials,
        supports_user_image_inputs: false, is_default: false
    }))];
}

export function typedCodingModelChoice(rows: readonly CodingProfile[], id: string | null | undefined): SavedCodingSelection | null {
    if (!id) return null;
    if (id === 'auto') return { kind: 'auto' };
    const row = rows.find(row => row.id === id && row.selectable !== false);
    const selection = row?.model_selection;
    const model = rows.find(candidate => candidate.engine === row?.engine && candidate.model_choice?.id === selection?.model_id)?.model_choice;
    if (!row?.engine || !selection || model?.choices[selection.thinking] !== id) {
        throw new Error('Your coding model choice is unavailable. Choose an engine and model to continue.');
    }
    return { kind: 'model', engine: row.engine, ...selection };
}
