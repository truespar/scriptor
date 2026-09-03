// Document comparison (blacklining), and the worker pool that runs it off the main thread.
//
// Comparing two .docx files is CPU-bound and can take seconds on a long contract, so it runs in a
// worker. Requests are sequenced and matched to their reply by id, which keeps one worker serving
// any number of concurrent callers.
import { compareDocx } from '@truespar/scriptor-wasm';

import { ensureWasm } from './wasm';

/** One difference in a comparison's manifest - the deterministic change set between two documents. */
export interface CompareChange {
  /** The tracked-change revision id in the redline (`0` for a table row / column op, which the OOXML
   *  layer does not surface an id for). */
  id: number;
  /** `insert` | `delete` | `replace` | `para-insert` | `para-delete` | `format` | `para-format` |
   *  `table-row-insert` | `table-row-delete` | `table-column-delete` | `move`. */
  kind: string;
  /** Paragraph index in the original document (document order). */
  para: number;
  /** Affected text on the original side (delete / replace / format / move / …). */
  before?: string;
  /** Affected text on the revised side (insert / replace / …). */
  after?: string;
}

/** One block of the original↔revised paragraph correspondence (side-by-side scroll-lock + highlight).
 *  `a` is the original paragraph index, `b` the revised; a side is absent for a pure insert / delete. */
export interface AlignEntry {
  a?: number;
  b?: number;
  /** `equal` (identical) | `edited` (both present, text differs) | `delete` (original only) |
   *  `insert` (revised only). */
  kind: 'equal' | 'edited' | 'delete' | 'insert';
}

/** The result of comparing two documents. */
export interface CompareResult {
  /** A Word-openable tracked-changes `.docx`: the original with every difference as an
   *  author-attributed tracked change. Open it with [`ScriptorView.loadDocx`] to review it - its
   *  changes then populate a reviewing pane bound to the view. */
  redline: Uint8Array;
  /** Every difference, in document order - the machine-readable manifest, provably consistent with
   *  the redline (accept-all reproduces the revised document, reject-all the original). */
  changes: CompareChange[];
  /** The paragraph-level original↔revised correspondence, in document order - drives the side-by-side
   *  view's semantic scroll-lock + per-paragraph highlighting. Empty when unavailable (the view then
   *  falls back to proportional scroll-sync). */
  alignment: AlignEntry[];
}

/**
 * A semantic annotation over a comparison change - the semantic overlay, supplied by the host's review
 * model (an LLM). It cites a change by its index in a [`CompareResult.changes`] and layers judgment on
 * it; it never alters the redline. `materiality` is the headline filter; `risks` are human-readable
 * flags (e.g. "'shall' -> 'may' weakens the obligation").
 */
export interface CompareAnnotation {
  /** Index of the annotated change in the comparison's `changes` (document order). */
  change: number;
  materiality: 'trivial' | 'substantive';
  category?: string;
  summary?: string;
  risks?: string[];
}

/** Build a `revision-id -> annotation` map from a comparison and its annotations - the key a
 *  [`ReviewingPane`] matches against (a review row carries the redline's tracked-change id). Changes
 *  with no revision id (a table row / column op) are skipped. */
export function compareAnnotationsById(
  result: CompareResult,
  annotations: CompareAnnotation[],
): Map<number, CompareAnnotation> {
  const byId = new Map<number, CompareAnnotation>();
  for (const a of annotations) {
    const change = result.changes[a.change];
    if (change && change.id > 0) byId.set(change.id, a);
  }
  return byId;
}

/** Knobs for a comparison. */
export interface CompareOptions {
  /** The single reviewer every emitted revision is attributed to. Default `"Compare"`. */
  author?: string;
  /** ISO-8601 date stamped on every revision. A parameter (the engine never reads a clock, so a fixed
   *  value gives a reproducible result); defaults to the current time. */
  date?: string;
  /** Detect + redline **formatting** changes (run/paragraph properties, style). `false` reports only
   *  content changes (Word / Litera "ignore formatting"). Default `true`. */
  detectFormatting?: boolean;
  /** Detect a paragraph **moved** verbatim as a move rather than delete + insert. Default `true`. */
  detectMoves?: boolean;
  /** Ignore whitespace-only differences (single vs. double space). Default `false`. */
  ignoreWhitespace?: boolean;
  /** Ignore letter-case-only differences. Default `false`. */
  ignoreCase?: boolean;
}

function parseCompare(raw: { redline: Uint8Array; manifest: string }): CompareResult {
  const manifest = JSON.parse(raw.manifest) as {
    changes?: CompareChange[];
    alignment?: AlignEntry[];
  };
  return {
    redline: raw.redline,
    changes: manifest.changes ?? [],
    alignment: manifest.alignment ?? [],
  };
}

interface CompareWorkerReply {
  id: number;
  redline?: Uint8Array;
  manifest?: string;
  error?: string;
}

// Compare runs a synchronous, potentially heavy wasm pass. On the main thread a large document would
// freeze the tab, so we run it in a Web Worker; `null` = not yet tried, `'off'` = unavailable (fall
// back to a synchronous main-thread compare). One worker is shared across compares.
let compareWorker: Worker | 'off' | null = null;
let compareSeq = 0;
const compareWaiters = new Map<
  number,
  { resolve: (r: CompareResult) => void; reject: (e: Error) => void }
>();

function getCompareWorker(): Worker | null {
  if (compareWorker === 'off') return null;
  if (compareWorker) return compareWorker;
  if (typeof Worker === 'undefined') {
    compareWorker = 'off';
    return null;
  }
  try {
    const w = new Worker(new URL('./compare-worker.ts', import.meta.url), { type: 'module' });
    w.addEventListener('message', (e: MessageEvent<CompareWorkerReply>) => {
      const { id, redline, manifest, error } = e.data;
      const waiter = compareWaiters.get(id);
      if (!waiter) return;
      compareWaiters.delete(id);
      if (error || !redline || manifest === undefined) {
        waiter.reject(new Error(error ?? 'comparison failed'));
      } else {
        waiter.resolve(parseCompare({ redline, manifest }));
      }
    });
    w.addEventListener('error', () => {
      // The worker itself failed (e.g. a bundler that didn't carry its wasm): reject anything pending
      // and fall back to a synchronous main-thread compare from here on.
      compareWorker = 'off';
      for (const waiter of compareWaiters.values()) {
        waiter.reject(new Error('compare worker failed'));
      }
      compareWaiters.clear();
    });
    compareWorker = w;
    return w;
  } catch {
    compareWorker = 'off';
    return null;
  }
}

/**
 * Compare two `.docx` documents and produce a **redline** (the original with every difference as an
 * author-attributed tracked change) plus the change manifest. Framework-agnostic and peer-independent
 * - the "what changed between these two files" path (a counterparty's returned markup, or two saved
 * versions). Runs in a Web Worker so a large comparison never freezes the UI (with a synchronous
 * main-thread fallback where Workers aren't available). Open [`CompareResult.redline`] with
 * [`ScriptorView.loadDocx`] to review it.
 */
export async function compareDocuments(
  original: Uint8Array,
  revised: Uint8Array,
  options: CompareOptions = {},
): Promise<CompareResult> {
  const author = options.author ?? 'Compare';
  // The engine takes the date as a parameter (it never reads a clock, so results stay deterministic);
  // real usage stamps the actual time. Callers wanting a fixed date for reproducibility pass one.
  const date = options.date ?? new Date().toISOString();
  // Detection controls (the comparison "profile"); the engine defaults are detect-all, ignore-none.
  const detectFormatting = options.detectFormatting ?? true;
  const detectMoves = options.detectMoves ?? true;
  const ignoreWhitespace = options.ignoreWhitespace ?? false;
  const ignoreCase = options.ignoreCase ?? false;

  const worker = getCompareWorker();
  if (worker) {
    const id = ++compareSeq;
    return new Promise<CompareResult>((resolve, reject) => {
      compareWaiters.set(id, { resolve, reject });
      // The inputs are structured-cloned (copied) to the worker, so the caller keeps its buffers.
      worker.postMessage({
        id,
        original,
        revised,
        author,
        date,
        detectFormatting,
        detectMoves,
        ignoreWhitespace,
        ignoreCase,
      });
    });
  }

  // Fallback: synchronous main-thread compare.
  await ensureWasm();
  const raw = compareDocx(
    original,
    revised,
    author,
    date,
    detectFormatting,
    detectMoves,
    ignoreWhitespace,
    ignoreCase,
  ) as { redline: Uint8Array; manifest: string };
  return parseCompare(raw);
}

