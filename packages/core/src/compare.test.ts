import { describe, expect, it } from 'vitest';

import { compareAnnotationsById } from './compare';
import type { CompareAnnotation, CompareResult } from './compare';

/** A manifest with the given change ids, in document order. */
function result(ids: number[]): CompareResult {
  return {
    redline: new Uint8Array(),
    changes: ids.map((id, i) => ({
      id,
      kind: 'insert',
      para: i,
      start: 0,
      end: 1,
      text: '',
      original: '',
    })),
    align: [],
  } as unknown as CompareResult;
}

const ann = (change: number, materiality: CompareAnnotation['materiality']): CompareAnnotation => ({
  change,
  materiality,
});

describe('compareAnnotationsById', () => {
  // Annotations arrive keyed by position in the manifest, but the reviewing pane looks changes up
  // by revision id. Getting this mapping wrong silently attaches a reviewer's note to the wrong
  // edit, which is worse than showing none.
  it('rekeys annotations from manifest index to revision id', () => {
    const map = compareAnnotationsById(result([11, 22, 33]), [
      ann(0, 'substantive'),
      ann(2, 'trivial'),
    ]);
    expect(map.get(11)?.materiality).toBe('substantive');
    expect(map.get(33)?.materiality).toBe('trivial');
    expect(map.has(22)).toBe(false);
  });

  it('drops annotations pointing past the manifest', () => {
    const map = compareAnnotationsById(result([11]), [ann(5, 'trivial')]);
    expect(map.size).toBe(0);
  });

  it('drops changes with no revision id', () => {
    // A table row or column operation reports id 0: the OOXML layer surfaces no revision id for it,
    // so there is nothing for the pane to bind an annotation to.
    const map = compareAnnotationsById(result([0, 42]), [ann(0, 'trivial'), ann(1, 'substantive')]);
    expect(map.size).toBe(1);
    expect(map.get(42)?.materiality).toBe('substantive');
  });

  it('lets a later annotation win for the same change', () => {
    const map = compareAnnotationsById(result([7]), [ann(0, 'trivial'), ann(0, 'substantive')]);
    expect(map.get(7)?.materiality).toBe('substantive');
  });
});
