// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { REQUEST_ROW_PEEK_PX, requestListHeight, requestListSizing } from './concurrentVoiceListSizing';

const bottoms = [46, 114, 160, 228, 274, 342];

describe('requestListHeight', () => {
  it('is exactly the content for four rows or fewer', () => {
    expect(requestListHeight(120, [46, 114])).toBe(120);
    expect(requestListHeight(234, bottoms.slice(0, 4))).toBe(234);
  });

  it('shows four measured rows and a peek when there are more', () => {
    expect(requestListHeight(348, bottoms)).toBe(228 + REQUEST_ROW_PEEK_PX);
    expect(requestListHeight(348, bottoms, 2000, 6)).toBe(228 + REQUEST_ROW_PEEK_PX);
  });

  it('yields to a short screen but keeps one row', () => {
    expect(requestListHeight(348, bottoms, 400, 6)).toBe(160);
    expect(requestListHeight(348, bottoms, 50, 6)).toBe(52);
  });

  it('is empty before anything is measured', () => {
    expect(requestListHeight(0, [], 400)).toBe(0);
    expect(requestListHeight(90, [])).toBe(90);
  });
});

describe('requestListSizing', () => {
  function list(rowHeights: number[], clientHeight: number) {
    const node = document.createElement('div');
    let top = 0;
    for (const height of rowHeights) {
      const row = document.createElement('div');
      row.className = 'request';
      Object.defineProperty(row, 'offsetTop', { value: top });
      Object.defineProperty(row, 'offsetHeight', { value: height });
      top += height;
      node.append(row);
    }
    Object.defineProperty(node, 'scrollHeight', { value: top });
    Object.defineProperty(node, 'clientHeight', { value: clientHeight });
    return node;
  }

  it('publishes the four-row cap and marks rows below the fold', () => {
    const node = list([46, 68, 46, 68, 46, 68], 246);
    const action = requestListSizing(node);
    expect(node.style.getPropertyValue('--request-list-cap')).toBe(`${228 + REQUEST_ROW_PEEK_PX}px`);
    expect(node.classList.contains('more-below')).toBe(true);
    action.destroy();
  });

  it('sizes a short list to its content with no fade', () => {
    const node = list([46, 68], 114);
    requestListSizing(node).destroy();
    expect(node.style.getPropertyValue('--request-list-cap')).toBe('114px');
    expect(node.classList.contains('more-below')).toBe(false);
  });
});
