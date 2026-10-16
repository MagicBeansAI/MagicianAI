import { describe, expect, it } from 'vitest';

import { liveTaskHasPlan, taskHasPlan } from './taskHasPlan';

describe('taskHasPlan', () => {
	it('is false for a quick chat task with no plan', () => {
		expect(taskHasPlan({})).toBe(false);
		expect(
			taskHasPlan({
				hasPlan: false,
				planStatus: undefined,
				latestPlanId: undefined,
				approvedPlanId: undefined,
				planSteps: []
			})
		).toBe(false);
		expect(taskHasPlan({ planStatus: null, latestPlanId: '', approvedPlanId: null, planSteps: null })).toBe(false);
	});

	it('is false for a missing task', () => {
		expect(taskHasPlan(null)).toBe(false);
		expect(taskHasPlan(undefined)).toBe(false);
	});

	it('is true when hasPlan is true', () => {
		expect(taskHasPlan({ hasPlan: true })).toBe(true);
	});

	it.each(['planning', 'draft', 'eliciting', 'approved', 'rejected', 'failed'])(
		'is true for any planStatus (%s)',
		(planStatus) => {
			expect(taskHasPlan({ planStatus })).toBe(true);
		}
	);

	it('is true with a latest plan id', () => {
		expect(taskHasPlan({ latestPlanId: 'plan_1' })).toBe(true);
	});

	it('is true with an approved plan id', () => {
		expect(taskHasPlan({ approvedPlanId: 'plan_1' })).toBe(true);
	});

	it('is true with at least one plan step', () => {
		expect(taskHasPlan({ planSteps: [{ description: 'step' }] })).toBe(true);
	});
});

describe('liveTaskHasPlan', () => {
	it('is false for a quick chat task with no plan', () => {
		expect(liveTaskHasPlan({})).toBe(false);
		expect(
			liveTaskHasPlan({
				messageStatus: 'running',
				storeStatus: 'completed',
				storePlan: { hasPlan: false, planSteps: [] },
				planSteps: []
			})
		).toBe(false);
	});

	it.each(['planning', 'draft', 'eliciting'])('is true when the message status is %s', (status) => {
		expect(liveTaskHasPlan({ messageStatus: status })).toBe(true);
	});

	it.each(['planning', 'draft', 'eliciting'])('is true when the store status is %s', (status) => {
		expect(liveTaskHasPlan({ messageStatus: 'running', storeStatus: status })).toBe(true);
	});

	it('is true when the store record has a plan', () => {
		expect(liveTaskHasPlan({ messageStatus: 'running', storePlan: { approvedPlanId: 'plan_1' } })).toBe(
			true
		);
		expect(liveTaskHasPlan({ storePlan: { planStatus: 'approved' } })).toBe(true);
	});

	it('is true when the card has plan steps', () => {
		expect(liveTaskHasPlan({ planSteps: ['Fetch', 'Summarise'] })).toBe(true);
	});
});
