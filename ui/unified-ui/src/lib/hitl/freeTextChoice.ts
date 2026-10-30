/**
 * Whether a choice option stands for "something else, in my words": one the
 * backend marks `requires_input`, or one named change/other (`Change…`,
 * `Other`). A one-click surface opens the prompt for it instead of posting
 * the bare id, so the words can go with it.
 */
export function isFreeTextChoiceOption(option: {
	id: string;
	label: string;
	requires_input?: boolean;
}): boolean {
	if (option.requires_input === true) return true;
	const named = (value: string) => /^(change|other)\b/i.test(value.trim());
	return named(option.id) || named(option.label);
}
