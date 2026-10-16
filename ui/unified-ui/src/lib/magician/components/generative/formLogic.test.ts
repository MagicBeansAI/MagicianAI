import { describe, expect, it } from 'vitest';
import {
	buildFormSchemaSignature,
	isRequiredFormValueEmpty,
	normalizeFormFieldOptions
} from './formLogic';

describe('formLogic.normalizeFormFieldOptions', () => {
	it('preserves explicit empty-string values', () => {
		const options = normalizeFormFieldOptions([
			{ value: '', label: 'None' },
			{ value: 'yes', label: 'Yes' }
		]);
		expect(options).toEqual([
			{ value: '', label: 'None' },
			{ value: 'yes', label: 'Yes' }
		]);
	});
});

describe('formLogic.buildFormSchemaSignature', () => {
	it('does not collide for delimiter-shaped option sets', () => {
		const left = buildFormSchemaSignature([
			{
				id: 'f1',
				type: 'select',
				min: null,
				max: null,
				step: null,
				options: [
					{ value: 'a,b', label: 'A' },
					{ value: 'c', label: 'C' }
				],
				defaultValue: ''
			}
		]);
		const right = buildFormSchemaSignature([
			{
				id: 'f1',
				type: 'select',
				min: null,
				max: null,
				step: null,
				options: [
					{ value: 'a', label: 'A' },
					{ value: 'b,c', label: 'C' }
				],
				defaultValue: ''
			}
		]);
		expect(left).not.toBe(right);
	});
});

describe('formLogic.isRequiredFormValueEmpty', () => {
	it('treats invalid number strings as empty for required checks', () => {
		expect(isRequiredFormValueEmpty('number', 'not-a-number')).toBe(true);
		expect(isRequiredFormValueEmpty('number', '')).toBe(true);
		expect(isRequiredFormValueEmpty('number', '42')).toBe(false);
		expect(isRequiredFormValueEmpty('number', 42)).toBe(false);
	});
});
