// Other people on the page: remote carets, their name chips, and comparison change markers.
//
// All of it is presentation over data the host pushes in - the view never derives a remote cursor,
// it is told about one. That is why this needs so little from the view: somewhere to draw, the page
// geometry to place a chip against, and the document to resolve an anchor that may have moved.

import type { ScriptorDoc } from '@truespar/scriptor-wasm';

import type { ChangeMarker, RemoteCursor } from './types';

/** What presence rendering needs from the view around it. */
export interface PresenceHost {
  readonly doc: ScriptorDoc;
  readonly sheet: HTMLDivElement;
  readonly octx: CanvasRenderingContext2D;
  readonly dpr: number;
  readonly deviceW: number;
  readonly deviceTotalH: number;
  readonly destroyed: boolean;
  drawOverlay(): void;
}

export class PresenceLayer {
  private readonly h: PresenceHost;

  constructor(host: PresenceHost) {
    this.h = host;
  }

  /** Remote collaborators' carets (presence), drawn on the overlay. Keyed-by-peer
   *  upserts arrive via `setRemoteCursors`; anchors are resolved at draw time. */
  remoteCursors: RemoteCursor[] = [];

  /** Comparison change bands (side-by-side review): a coloured row behind each changed paragraph,
   *  painted on the overlay. Set via [`setChangeMarkers`]; empty for a normal view. */
  changeMarkers: ChangeMarker[] = [];

  // Optional author-name -> avatar-URL map, so the change tooltip can show who
  // made a change as a face, not just a name. Host-supplied (setAuthorAvatars).
  authorAvatars = new Map<string, string>();

  // Optional author-id -> display-name map, so a change stamped with an internal
  // actor id (e.g. `max@0`) renders as a human name ("Max") in the tooltip /
  // accept-reject popup / comment thread. Host-supplied (setAuthorNames).
  authorNames = new Map<string, string>();

  // Live "presence chips" for remote carets that carry a `state` (an agent at
  // work): a DOM overlay (avatar + "{name} is thinking…/typing…") keyed by peer
  // id, positioned at the caret in client coords each frame and removed when the
  // peer is no longer drawn. See {@link drawRemoteCursors}.
  presenceChips = new Map<string, HTMLDivElement>();

  presenceStyleInjected = false;

  /** Upsert the set of remote collaborators' carets (by peer id) and redraw the
   *  overlay. Anchors are resolved against the live document each draw, so remote
   *  carets follow their content as the local user edits. */
  setRemoteCursors(cursors: RemoteCursor[]): void {
    this.remoteCursors = cursors;
    if (!this.h.destroyed) this.h.drawOverlay();
  }

  /** Mark changed paragraphs with coloured bands on the overlay - the side-by-side comparison view's
   *  per-change highlighting (so a page of several changes shows them all, the active one emphasized).
   *  Replaces any previous set; pass `[]` to clear. Repaints (bands follow scroll for free). */
  setChangeMarkers(markers: ChangeMarker[]): void {
    this.changeMarkers = markers;
    if (!this.h.destroyed) this.h.drawOverlay();
  }

  /** Provide a map of author display-name -> avatar URL so the tracked-change
   *  hover tooltip can show the author's face beside their name. Keyed by the
   *  same `w:author` string a change carries. Unknown authors just show no
   *  avatar. Safe to call repeatedly (e.g. as the collaborator set changes). */
  setAuthorAvatars(map: Record<string, string>): void {
    this.authorAvatars = new Map(Object.entries(map));
  }

  /** Provide a map of author-id -> display name (keyed by the `w:author` string a
   *  change carries, e.g. `max@0` -> "Max"), so tracked-change chrome shows a
   *  human name rather than an internal actor id. Unknown authors render as-is. */
  setAuthorNames(map: Record<string, string>): void {
    this.authorNames = new Map(Object.entries(map));
  }

  /** Resolve a stored author string to its display name (falls back to itself). */
  displayAuthor(author: string): string {
    return this.authorNames.get(author) ?? author;
  }

  /** Paint remote collaborators' carets: resolve each peer's anchor to a current
   *  position, then draw a colored bar + name tag. Anchors that no longer resolve
   *  (deleted block) are skipped. */
  drawRemoteCursors(): void {
    // Peers that got a presence chip this frame; any chip not in this set is
    // stale (the peer left, lost its anchor, or stopped) and gets removed below.
    const drawnChips = new Set<string>();
    if (this.remoteCursors.length) {
      const barW = Math.max(2, this.h.dpr * 2);
      const fontPx = Math.round(11 * this.h.dpr);
      for (const rc of this.remoteCursors) {
        try {
          const pos = this.h.doc.resolveAnchor(rc.anchor);
          if (!pos || pos.length !== 2) continue;
          const c = this.h.doc.caretRect(pos[0]!, pos[1]!);
          if (!c || c.length < 3) continue;
          const x = c[0]!;
          const y = c[1]!;
          const h = c[2]!;
          // The colored caret bar is always drawn on canvas, human or agent.
          this.h.octx.fillStyle = rc.color;
          this.h.octx.fillRect(x, y, barW, h);
          if (rc.state) {
            // An agent at work: a DOM presence chip at the caret in client
            // coords. Replaces the canvas name-tag (it carries the name itself).
            this.placePresenceChip(rc, pos[0]!, pos[1]!);
            drawnChips.add(rc.peer);
          } else if (rc.name) {
            // An ordinary human collaborator: canvas name-tag, unchanged.
            this.h.octx.font = `${fontPx}px sans-serif`;
            const tagH = fontPx + 4 * this.h.dpr;
            const tagY = Math.max(0, y - tagH);
            const tw = this.h.octx.measureText(rc.name).width;
            this.h.octx.fillStyle = rc.color;
            this.h.octx.fillRect(x, tagY, tw + 8 * this.h.dpr, tagH);
            this.h.octx.fillStyle = '#ffffff';
            this.h.octx.textBaseline = 'top';
            this.h.octx.fillText(rc.name, x + 4 * this.h.dpr, tagY + 2 * this.h.dpr);
          }
        } catch {
          // A cursor we can't place this frame (e.g. mid-merge); skip it.
        }
      }
    }
    // Drop chips for peers not drawn this frame (left / no state / unresolved).
    for (const [peer, el] of this.presenceChips) {
      if (!drawnChips.has(peer)) {
        el.remove();
        this.presenceChips.delete(peer);
      }
    }
  }

  /** One-time injection of the presence-chip keyframes (animated trailing dots). */
  ensurePresenceStyle(): void {
    if (this.presenceStyleInjected) return;
    this.presenceStyleInjected = true;
    if (document.getElementById('scriptor-presence-style')) return;
    const style = document.createElement('style');
    style.id = 'scriptor-presence-style';
    style.textContent =
      '@keyframes scriptor-presence-dots{0%,20%{opacity:.2}40%{opacity:1}100%{opacity:.2}}' +
      '.scriptor-presence-chip .scriptor-presence-dot{animation:scriptor-presence-dots 1.2s infinite}' +
      '.scriptor-presence-chip .scriptor-presence-dot:nth-child(2){animation-delay:.2s}' +
      '.scriptor-presence-chip .scriptor-presence-dot:nth-child(3){animation-delay:.4s}';
    document.head.append(style);
  }

  /** Create / update an agent's presence chip (avatar + "{name} is thinking…/typing…")
   *  and position it at the caret `(para, off)` in client coords, mirroring the
   *  overlay-device-px -> client mapping in {@link caretClientXY}. */
  placePresenceChip(rc: RemoteCursor, para: number, off: number): void {
    this.ensurePresenceStyle();
    let chip = this.presenceChips.get(rc.peer);
    if (!chip) {
      chip = document.createElement('div');
      chip.className = 'scriptor-presence-chip';
      chip.style.cssText =
        'position:fixed;z-index:2147483647;pointer-events:none;display:flex;align-items:center;gap:6px;' +
        'padding:3px 9px 3px 3px;border-radius:999px;color:#fff;white-space:nowrap;' +
        'font:12px system-ui,-apple-system,sans-serif;box-shadow:0 2px 10px rgba(0,0,0,.20);';
      document.body.append(chip);
      this.presenceChips.set(rc.peer, chip);
    }
    chip.style.background = rc.color;
    // Rebuild contents each frame (cheap; the verb / avatar can change between frames).
    chip.replaceChildren();
    const avatarUrl = this.authorAvatars.get(rc.name);
    if (avatarUrl) {
      const img = document.createElement('img');
      img.src = avatarUrl;
      img.alt = '';
      img.style.cssText =
        'width:18px;height:18px;border-radius:50%;object-fit:cover;flex:0 0 auto;background:rgba(255,255,255,.25);';
      chip.append(img);
    } else {
      const circle = document.createElement('div');
      circle.style.cssText =
        'width:18px;height:18px;border-radius:50%;flex:0 0 auto;display:flex;align-items:center;' +
        'justify-content:center;background:rgba(255,255,255,.28);font-size:10px;font-weight:600;line-height:1;';
      circle.textContent = (rc.name.trim()[0] ?? '?').toUpperCase();
      chip.append(circle);
    }
    const label = document.createElement('span');
    const verb = rc.state === 'typing' ? 'is typing' : 'is thinking';
    label.append(document.createTextNode(`${rc.name} ${verb}`));
    for (let i = 0; i < 3; i++) {
      const dot = document.createElement('span');
      dot.className = 'scriptor-presence-dot';
      dot.textContent = '.';
      label.append(dot);
    }
    chip.append(label);
    // Position the chip just above-right of the caret, in client coords.
    const r = this.h.sheet.getBoundingClientRect();
    const sx = r.width / Math.max(1, this.h.deviceW);
    const sy = r.height / Math.max(1, this.h.deviceTotalH);
    const cr = this.h.doc.caretRect(para, off); // [x, y, h] device px in document space
    const left = r.left + cr[0]! * sx + 6;
    const top = r.top + cr[1]! * sy - 26;
    chip.style.left = `${left}px`;
    chip.style.top = `${Math.max(0, top)}px`;
  }

  /** Tear down every presence chip (peer left / view destroyed). */
  clearPresenceChips(): void {
    for (const el of this.presenceChips.values()) el.remove();
    this.presenceChips.clear();
  }
}
