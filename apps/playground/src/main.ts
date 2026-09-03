/**
 * Scriptor playground - the full Word-style editor via @truespar/scriptor-ui's Workspace (ribbon +
 * rulers + status bar around the headless canvas core). This app only adds a slim title bar with
 * file / mode controls; the Workspace owns the rest.
 */

import type { CompareAnnotation, CompareResult, ScriptorMode } from '@truespar/scriptor-core';
import { Workspace } from '@truespar/scriptor-ui';
import './style.css';

/**
 * Demo semantic-overlay annotator - a *deterministic heuristic* standing in for the review model a
 * real host wires here (an LLM reading the manifest). It flags changes touching obligations, money,
 * or dates as substantive, raises a risk on a "shall" -> "may" weakening, and marks whole-clause
 * moves / insert / delete as substantive. It never alters the redline - it only describes it.
 */
function demoAnnotate(result: CompareResult): CompareAnnotation[] {
  const money = /[$£€]|\b\d[\d,]*(\.\d+)?\b/;
  const date = /\b(days?|weeks?|months?|years?|\d{1,2}\/\d{1,2}\/\d{2,4})\b/i;
  return result.changes.map((c, i): CompareAnnotation => {
    const before = (c.before ?? '').toLowerCase();
    const after = (c.after ?? '').toLowerCase();
    const both = `${before} ${after}`;
    const risks: string[] = [];
    let materiality: 'trivial' | 'substantive' = 'trivial';
    let category = '';
    let summary = '';
    if (/\bshall\b/.test(before) && /\b(may|might)\b/.test(after)) {
      materiality = 'substantive';
      category = 'obligation';
      summary = 'Weakens a mandatory obligation to a permissive one.';
      risks.push("'shall' → 'may' weakens the obligation");
    } else if (/\b(shall|must|may|will|obligation|indemnif|terminat|liab)\b/.test(both)) {
      materiality = 'substantive';
      category = 'obligation';
    } else if (money.test(both)) {
      materiality = 'substantive';
      category = 'money';
      summary = 'Changes a monetary figure.';
    } else if (date.test(both)) {
      materiality = 'substantive';
      category = 'date';
    } else if (c.kind === 'move') {
      materiality = 'substantive';
      category = 'relocation';
    } else if (c.kind === 'para-insert' || c.kind === 'para-delete') {
      materiality = 'substantive';
      category = c.kind === 'para-insert' ? 'clause added' : 'clause removed';
    }
    return { change: i, materiality, category, summary, risks };
  });
}

const app = document.getElementById('app');
if (!app) throw new Error('missing #app');

app.innerHTML = `
  <header class="bar">
    <strong>Scriptor</strong><span class="tag">canvas · word ui</span>
    <div class="tools">
      <label class="btn">Open .docx<input id="file" type="file"
        accept=".docx,application/vnd.openxmlformats-officedocument.wordprocessingml.document" hidden></label>
      <button id="new" class="btn">New</button>
      <button id="snapshot" class="btn">Snapshot</button>
      <button id="mode" class="btn">Mode: edit</button>
    </div>
    <span id="status" class="status">loading…</span>
  </header>
  <div id="frame"></div>
`;

const status = document.getElementById('status') as HTMLElement;
// The status pill ellipsizes on narrow bars - mirror its text into `title` so hover shows it all.
new MutationObserver(() => {
  status.title = status.textContent ?? '';
}).observe(status, { childList: true, characterData: true, subtree: true });
const frame = document.getElementById('frame') as HTMLElement;
const modeBtn = document.getElementById('mode') as HTMLButtonElement;

let mode: ScriptorMode = 'edit';

// In-session version history: the Snapshot button captures the current document; the Compare dialog's
// version-picker offers these as sources. A real host backs this with its version store.
const snapshots: { id: string; label: string; bytes: Uint8Array }[] = [];

Workspace.create(frame, {
  mode,
  // The demo review model - a real host wires an LLM here to annotate a comparison's changes.
  compareAnnotate: demoAnnotate,
  // The version-aware compare path: offer the captured snapshots as pickable documents.
  compareVersions: () => snapshots.map((s) => ({ id: s.id, label: s.label, load: () => s.bytes })),
  onReady: () => {
    status.textContent =
      'ready - open a .docx or just type · Review tab: Track Changes, accept/reject, and Compare two documents · right-click a table cell for rows & columns';
  },
})
  .then((ws) => {
    document.getElementById('file')?.addEventListener('change', async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;
      status.textContent = `opening ${file.name}…`;
      try {
        ws.loadDocx(new Uint8Array(await file.arrayBuffer()));
        status.textContent = file.name;
      } catch (err) {
        status.textContent = `error: ${err}`;
        console.error(err);
      }
    });

    document.getElementById('new')?.addEventListener('click', () => {
      ws.newDocument();
      status.textContent = 'new empty document';
    });

    document.getElementById('snapshot')?.addEventListener('click', () => {
      const label = `Snapshot ${snapshots.length + 1} (${new Date().toLocaleTimeString()})`;
      snapshots.push({ id: `snap-${snapshots.length}`, label, bytes: ws.view.toDocx() });
      status.textContent = `captured ${label} - edit, then Review → Compare against it`;
    });

    // Save now lives in the editor's Quick Access Toolbar (+ Ctrl/Cmd+S); with no onSave handler it
    // falls back to a .docx download, so the playground needs no Save button of its own.

    modeBtn.addEventListener('click', () => {
      mode = mode === 'edit' ? 'read' : 'edit';
      ws.view.setMode(mode);
      modeBtn.textContent = `Mode: ${mode}`;
    });
  })
  .catch((err) => {
    status.textContent = `init failed: ${err}`;
    console.error(err);
  });
