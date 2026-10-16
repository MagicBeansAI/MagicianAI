/** Presentation-only identity defaults. Never import backend service keys here. */
import { ASSISTANT_FALLBACK_NAME } from './presentationIdentity.generated';

export {
	ASSISTANT_FALLBACK_NAME,
	HOST_APP_NAME,
	PRODUCT_NAME
} from './presentationIdentity.generated';

export function agentDisplayName(
	agent: { name?: string | null; aliases?: string[] | null } | null | undefined
): string {
	const name = agent?.name?.trim();
	if (name) return name;
	const alias = agent?.aliases?.find((value) => value.trim())?.trim();
	return alias || ASSISTANT_FALLBACK_NAME;
}
