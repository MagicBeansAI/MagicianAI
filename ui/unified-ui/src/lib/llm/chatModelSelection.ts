import { selectionForRoute, type ModelChoice, type ModelSelection } from './modelCatalog';

/** Ephemeral picker rows derived entirely from the model catalog. */
export interface ChatModelRow {
    name: string;
    provider: string;
    model: string;
    is_default: boolean;
    supports_user_image_inputs: boolean;
    model_selection: ModelSelection;
    model_choice: ModelChoice | null;
}

export function chatModelRows(models: readonly ModelChoice[], initial: ModelSelection | null): ChatModelRow[] {
    return models.flatMap(model => Object.entries(model.choices).map(([thinking, name]) => ({
        name, provider: model.provider, model: model.model,
        is_default: initial?.model_id === model.id && initial.thinking === thinking,
        supports_user_image_inputs: model.capabilities.vision,
        model_selection: { model_id: model.id, thinking },
        model_choice: thinking === 'auto' ? model : null
    })));
}

/** Used by normal, streaming, queue and concurrent-voice admissions alike. */
export function typedChatModelRequest(
    models: readonly ModelChoice[], rows: readonly { name: string; model_selection?: ModelSelection | null }[],
    route: string | null | undefined, engine: string, nativeModel = 'default'
): Record<string, unknown> {
    if (!['magician', 'pi'].includes(engine)) return { harness_engine: engine, harness_model: nativeModel };
    const selection = selectionForRoute(models, rows, route);
    if (!selection || !models.some(model => model.id === selection.model_id && model.choices[selection.thinking] === route)) {
        throw new Error('Your saved model choice is unavailable. Choose a model to continue.');
    }
    return { harness_engine: engine, model_selection: selection };
}
