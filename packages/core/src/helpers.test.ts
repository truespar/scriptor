import { describe, expect, it } from 'vitest';

import {
  BODY_PARA_LIMIT,
  FOOTER_PARA_BASE,
  changeAccentColor,
  changeBandColor,
  formatCommentDate,
  regionOf,
  sanitizeBookmarkName,
} from './helpers';

describe('regionOf', () => {
  // The engine namespaces the three stories into disjoint paragraph ranges rather than tagging
  // paragraphs, so this arithmetic is what tells a body caret from a header one. Getting it wrong
  // routes an edit into the wrong document, which is why the boundaries are pinned exactly.
  it('splits body, header and footer at the namespace boundaries', () => {
    expect(regionOf(0)).toBe(0);
    expect(regionOf(BODY_PARA_LIMIT - 1)).toBe(0);
    expect(regionOf(BODY_PARA_LIMIT)).toBe(1);
    expect(regionOf(FOOTER_PARA_BASE - 1)).toBe(1);
    expect(regionOf(FOOTER_PARA_BASE)).toBe(2);
    expect(regionOf(FOOTER_PARA_BASE + 5000)).toBe(2);
  });
});

describe('sanitizeBookmarkName', () => {
  it('keeps a name Word already accepts', () => {
    expect(sanitizeBookmarkName('Clause_7')).toBe('Clause_7');
  });

  it('replaces characters Word rejects', () => {
    expect(sanitizeBookmarkName('clause 7.2 (a)')).toBe('clause_7_2__a_');
  });

  it('prefixes a name that does not start with a letter', () => {
    // OOXML requires a letter first; a leading digit or underscore would be rejected on open.
    expect(sanitizeBookmarkName('7.2')).toBe('B7_2');
    expect(sanitizeBookmarkName('_intro')).toBe('B_intro');
  });

  it('truncates to Word's 40-character limit', () => {
    expect(sanitizeBookmarkName('a'.repeat(60))).toHaveLength(40);
  });

  it('returns empty for input with nothing usable', () => {
    expect(sanitizeBookmarkName('   ')).toBe('');
    expect(sanitizeBookmarkName('')).toBe('');
  });
});

describe('formatCommentDate', () => {
  it('passes through an empty or unparseable stamp rather than showing Invalid Date', () => {
    expect(formatCommentDate('')).toBe('');
    expect(formatCommentDate('not a date')).toBe('not a date');
  });

  it('formats a real ISO stamp to something short', () => {
    const out = formatCommentDate('2026-03-04T09:05:00Z');
    expect(out).not.toBe('2026-03-04T09:05:00Z');
    expect(out.length).toBeGreaterThan(0);
    expect(out).not.toMatch(/Invalid/);
  });
});

describe('change marker colours', () => {
  it('gives each kind its own hue and strengthens the focused one', () => {
    const insert = changeBandColor('insert', false);
    const del = changeBandColor('delete', false);
    expect(insert).not.toBe(del);
    // The focused band is the same hue, painted stronger.
    expect(changeBandColor('insert', true)).not.toBe(insert);
    expect(changeAccentColor('insert')).toMatch(/^rgba\(/);
  });
});
