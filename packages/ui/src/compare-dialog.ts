import { type CompareResult, compareDocuments, type ScriptorView } from '@truespar/scriptor-core';
import { h, injectStyles } from './dom';
import { icon } from './icons';

/** A named document the user can compare against - typically a saved version from the host's store.
 *  `load` fetches its `.docx` bytes lazily (only if the user picks it). */
export interface CompareVersion {
  id: string;
  label: string;
  load: () => Uint8Array | Promise<Uint8Array>;
}

export interface CompareDialogOptions {
  /** Named documents (e.g. saved versions) the user can pick for either side, in addition to the
   *  current document and a dropped / browsed file. Resolved once when the dialog opens - the
   *  version-aware path (compare the current document against a previous version, no file picking). */
  versions?: () => CompareVersion[] | Promise<CompareVersion[]>;
}

/** A pickable document slot: choose the current document, a named version, or a file (drag-drop /
 *  browse), and yield the chosen `.docx` as bytes. */
interface DocSlot {
  el: HTMLElement;
  bytes(): Promise<Uint8Array | null>;
  /** Whether the current selection is the open document (so the redline can be attributed to the
   *  review author). */
  usingCurrent(): boolean;
  /** A human label for the chosen source (the file / version name, or "Current document") - shown in
   *  the side-by-side headers. */
  name(): string;
}

/** What a slot starts out holding: the open document, a saved version, or nothing. */
type SlotInitial = { kind: 'current' } | { kind: 'empty' } | { kind: 'version'; index: number };

/**
 * One side of the comparison as a **card**: it shows the chosen document (the open document, a dropped
 * / browsed file, or a saved version) or, when empty, a drop target. Drag a `.docx` onto it (or
 * "browse") to set it; the open document + any versions are offered as chips so the common case needs
 * no dropdown. `current` is the open document's byte source; `initial` is what the slot starts
 * holding (a prefilled slot is one click away from a different choice via its clear button).
 */
function docSlot(
  current: (() => Uint8Array) | null,
  versions: CompareVersion[],
  initial: SlotInitial,
): DocSlot {
  let chosen: File | null = null;
  let versionIdx = initial.kind === 'version' ? initial.index : -1;
  // 'current' (the open document) | 'file' (dropped/browsed) | 'version' | 'empty'.
  let source: 'current' | 'file' | 'version' | 'empty' =
    initial.kind === 'current' && current ? 'current' : initial.kind === 'version' ? 'version' : 'empty';

  const card = h('div', { class: 'scr-cmp-card' });
  const input = h('input', {
    type: 'file',
    accept: '.docx',
    class: 'scr-drop-input',
  }) as HTMLInputElement;

  const pick = (f: File | null): void => {
    if (!f) return;
    chosen = f;
    source = 'file';
    render();
  };

  // A chosen document: an icon, its name, a sub-label, and a clear (×) to pick a different one.
  const filled = (name: string, sub: string, onClear: () => void): HTMLElement => {
    const el = h('div', { class: 'scr-cmp-filled' });
    const txt = h('div', { class: 'scr-cmp-filled-txt' });
    const nm = h('div', { class: 'scr-cmp-name' });
    nm.textContent = name;
    const mt = h('div', { class: 'scr-cmp-sub' });
    mt.textContent = sub;
    txt.append(nm, mt);
    const clear = h('button', {
      class: 'scr-cmp-clear',
      type: 'button',
      title: 'Choose a different document',
    }) as HTMLButtonElement;
    clear.append(icon('reject', 14));
    clear.addEventListener('click', onClear);
    el.append(icon('page', 22), txt, clear);
    return el;
  };

  // Empty: the drop target + a "browse" and the secondary source chips (open document / versions).
  const empty = (): HTMLElement => {
    const el = h('div', { class: 'scr-cmp-drop' });
    const label = h('div', { class: 'scr-cmp-drop-label' });
    label.textContent = 'Drop a .docx here';
    const browse = h('button', { class: 'scr-drop-browse', type: 'button' }) as HTMLButtonElement;
    browse.textContent = 'or browse';
    browse.addEventListener('click', (e) => {
      e.stopPropagation();
      input.click();
    });
    el.append(label, browse);
    const alts = h('div', { class: 'scr-cmp-alts' });
    if (current) {
      const b = h('button', { class: 'scr-cmp-alt', type: 'button' }) as HTMLButtonElement;
      b.textContent = 'Current document';
      b.addEventListener('click', () => {
        source = 'current';
        render();
      });
      alts.append(b);
    }
    versions.forEach((v, i) => {
      const b = h('button', { class: 'scr-cmp-alt', type: 'button' }) as HTMLButtonElement;
      b.textContent = v.label;
      b.addEventListener('click', () => {
        source = 'version';
        versionIdx = i;
        render();
      });
      alts.append(b);
    });
    if (alts.childElementCount) el.append(alts);
    return el;
  };

  function render(): void {
    card.replaceChildren(input);
    card.classList.toggle('scr-cmp-card-filled', source !== 'empty');
    if (source === 'current' && current) {
      // Clearing the open document drops to the empty state so a file can be picked instead.
      card.append(
        filled('Current document', 'the open document', () => {
          source = 'empty';
          render();
        }),
      );
    } else if (source === 'file' && chosen) {
      card.append(
        filled(chosen.name, 'file', () => {
          source = current ? 'current' : 'empty';
          chosen = null;
          render();
        }),
      );
    } else if (source === 'version' && versionIdx >= 0) {
      card.append(
        filled(versions[versionIdx]?.label ?? 'Version', 'saved version', () => {
          source = current ? 'current' : 'empty';
          versionIdx = -1;
          render();
        }),
      );
    } else {
      card.append(empty());
    }
  }

  input.addEventListener('change', () => pick(input.files?.[0] ?? null));
  card.addEventListener('dragover', (e) => {
    e.preventDefault();
    card.classList.add('scr-drop-over');
  });
  card.addEventListener('dragleave', () => card.classList.remove('scr-drop-over'));
  card.addEventListener('drop', (e) => {
    e.preventDefault();
    card.classList.remove('scr-drop-over');
    pick(e.dataTransfer?.files?.[0] ?? null);
  });
  render();

  return {
    el: card,
    async bytes() {
      if (source === 'current' && current) return current();
      if (source === 'file') return chosen ? new Uint8Array(await chosen.arrayBuffer()) : null;
      if (source === 'version' && versionIdx >= 0) {
        const v = versions[versionIdx];
        return v ? new Uint8Array(await v.load()) : null;
      }
      return null;
    },
    usingCurrent: () => source === 'current',
    name() {
      if (source === 'current') return 'Current document';
      if (source === 'file') return chosen?.name ?? 'File';
      if (source === 'version' && versionIdx >= 0) return versions[versionIdx]?.label ?? 'Version';
      return '';
    },
  };
}

/**
 * Open the **Compare Documents** dialog: pick an *Original* and a *Revised* (the open document, a
 * saved version, or a file - prefilled from context), run the comparison, and hand the
 * [`CompareResult`] (redline `.docx` + change manifest) to `onResult`. A centered modal (backdrop /
 * Esc / Cancel dismiss). When the original is the current document the redline is attributed to the
 * view's review author; otherwise to "Compare".
 *
 * The host decides what to do with the result - the [`Workspace`] opens the redline in its view (its
 * tracked changes then populate the reviewing pane).
 */
/** The two source documents a comparison ran on (the exact bytes diffed), handed to `onResult`
 *  alongside the result so the host can render them side-by-side - with a display name for each. */
export interface CompareSources {
  original: Uint8Array;
  revised: Uint8Array;
  originalName: string;
  revisedName: string;
}

export function openCompareDialog(
  view: ScriptorView,
  onResult: (result: CompareResult, sources: CompareSources) => void,
  options: CompareDialogOptions = {},
): void {
  void Promise.resolve(options.versions?.() ?? []).then((versions) =>
    buildDialog(view, onResult, versions),
  );
}

function buildDialog(
  view: ScriptorView,
  onResult: (result: CompareResult, sources: CompareSources) => void,
  versions: CompareVersion[],
): void {
  injectStyles();

  const backdrop = h('div', { class: 'scr-modal-backdrop' });
  const card = h('div', { class: 'scr-modal scr-compare-modal' });
  backdrop.append(card);
  const title = h('div', { class: 'scr-modal-title' });
  title.textContent = 'Compare Documents';
  card.append(title);

  // Two columns, Original → Revised, mirroring the side-by-side result the comparison produces (so the
  // user pictures the diff before running it). Defaults follow the flow that brought the user here:
  // with saved versions, prefill the snapshot flow - Original = the latest version (the older
  // document), Revised = the open document - so Compare works in one click; without versions,
  // Original = the open document and Revised awaits a file. Both slots offer the current-document
  // chip, so the pairing can be flipped.
  const cur = (): Uint8Array => view.toDocx();
  const original = docSlot(
    cur,
    versions,
    versions.length ? { kind: 'version', index: versions.length - 1 } : { kind: 'current' },
  );
  const revised = docSlot(cur, versions, versions.length ? { kind: 'current' } : { kind: 'empty' });
  const column = (label: string, slot: DocSlot): HTMLElement => {
    const c = h('div', { class: 'scr-cmp-col' });
    const head = h('div', { class: 'scr-cmp-col-head' });
    head.textContent = label;
    c.append(head, slot.el);
    return c;
  };
  const cols = h('div', { class: 'scr-cmp-cols' });
  const arrow = h('div', { class: 'scr-cmp-arrow' });
  arrow.append(icon('next', 18));
  cols.append(column('Original', original), arrow, column('Revised', revised));
  card.append(cols);

  // Detection controls (the comparison "profile"). Engine defaults: detect all, ignore none.
  const opts = {
    detectFormatting: true,
    detectMoves: true,
    ignoreWhitespace: false,
    ignoreCase: false,
  };
  const optsRow = h('div', { class: 'scr-cmp-opts' });
  const optToggle = (text: string, key: keyof typeof opts): HTMLElement => {
    const l = h('label', { class: 'scr-cmp-opt' });
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.checked = opts[key];
    cb.addEventListener('change', () => {
      opts[key] = cb.checked;
    });
    const s = document.createElement('span');
    s.textContent = text;
    l.append(cb, s);
    return l;
  };
  optsRow.append(
    optToggle('Formatting', 'detectFormatting'),
    optToggle('Moves', 'detectMoves'),
    optToggle('Ignore whitespace', 'ignoreWhitespace'),
    optToggle('Ignore case', 'ignoreCase'),
  );
  card.append(optsRow);

  const err = h('div', { class: 'scr-compare-err' });
  card.append(err);

  const actions = h('div', { class: 'scr-modal-actions' });
  const cancel = h('button', { class: 'scr-modal-btn', type: 'button' }) as HTMLButtonElement;
  cancel.textContent = 'Cancel';
  const run = h('button', {
    class: 'scr-modal-btn scr-modal-primary',
    type: 'button',
  }) as HTMLButtonElement;
  run.textContent = 'Compare';
  actions.append(cancel, run);
  card.append(actions);

  const close = (): void => {
    backdrop.remove();
    document.removeEventListener('keydown', onKey, true);
  };
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    }
  };
  backdrop.addEventListener('mousedown', (e) => {
    if (e.target === backdrop) close();
  });
  document.addEventListener('keydown', onKey, true);
  cancel.addEventListener('click', close);

  run.addEventListener('click', () => {
    void (async () => {
      err.textContent = '';
      const [o, r] = await Promise.all([original.bytes(), revised.bytes()]);
      if (!o || !r) {
        err.textContent = 'Choose both an original and a revised document.';
        return;
      }
      run.disabled = true;
      run.textContent = 'Comparing…';
      try {
        // When the original is the open document, attribute the redline to the review author, and
        // use the exact bytes `compareWith` diffs (the loaded file, not a re-serialization) as the
        // side-by-side original - so its paragraph indices line up with the alignment.
        const usingCurrent = original.usingCurrent();
        const result = usingCurrent
          ? await view.compareWith(r, opts)
          : await compareDocuments(o, r, opts);
        const originalBytes = usingCurrent ? view.comparisonSource() : o;
        close();
        onResult(result, {
          original: originalBytes,
          revised: r,
          originalName: original.name(),
          revisedName: revised.name(),
        });
      } catch (e) {
        err.textContent = `Comparison failed: ${(e as Error).message}`;
        run.disabled = false;
        run.textContent = 'Compare';
      }
    })();
  });

  document.body.append(backdrop);
}
