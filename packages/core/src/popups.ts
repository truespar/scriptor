// Popup chrome: menus, tooltips, the comment popover and the single-line input dialog.
//
// This is the DOM half of the view's chrome - building an element, placing it inside the viewport,
// wiring its dismissal, and tearing it down. What the menus CONTAIN stays in the view, because that
// is the command surface rather than presentation, which is why the contract below is small.
//
// Every popup here is mounted on `document.body` rather than inside the page, so it is never
// clipped by the scrolling stage, and every one clamps itself into the viewport.

import type { ScriptorDoc } from '@truespar/scriptor-wasm';

import { formatCommentDate, TRACK_NOUN, TRACK_VERB } from './helpers';
import type {
  CaretPos,
  CommentJson,
  InputDialogOptions,
  ScriptorContextMenuItem,
  Selection,
} from './types';

/** What the popup layer needs from the view around it. */
export interface PopupHost {
  readonly doc: ScriptorDoc;
  readonly keys: HTMLTextAreaElement;
  readonly authorAvatars: Map<string, string>;
  sel: Selection | null;
  /** The hover the tooltip is currently showing for. Shared: `onTrackHover` (which stays in the
   *  view, because it is hit-testing rather than chrome) sets it, and dismissing the tip clears it. */
  lastHoverKey: string;
  displayAuthor(author: string): string;
  caretClientXY(para: number, off: number): { x: number; y: number };
  afterResolve(): void;
  resolveRevision(para: number, id: number, accept: boolean): void;
}

export class PopupLayer {
  private readonly h: PopupHost;

  constructor(host: PopupHost) {
    this.h = host;
  }

  tableMenuEl: HTMLDivElement | null = null;

  onDocPointer: ((e: Event) => void) | null = null;

  // Inline tracked-change chrome: a hover tooltip + a click accept/reject popup (1C).
  trackTip: HTMLDivElement | null = null;

  trackPopupEl: HTMLDivElement | null = null;

  trackPopupCleanup: (() => void) | null = null;

  // Comment chrome: a click-to-open thread/compose popover + cached anchor-highlight rects (refreshed
  // on render, reused while dragging the selection so a drag doesn't rescan the whole document).
  commentPopupEl: HTMLDivElement | null = null;

  commentPopupCleanup: (() => void) | null = null;

  /** Render a context menu at fixed `(x, y)` from a [`ScriptorContextMenuItem`] list (a separator
   *  is `{ separator }` or a dash label with no `onClick`; a disabled item renders greyed). Dismisses
   *  on an outside click or Escape. The `[label, action|null]` tuple form is still accepted for the
   *  table/picture menus (converted here). */
  /** Clamp a fixed-position popup fully into the viewport once its rendered size is known (next
   *  frame), with an 8px inset from every edge - a context menu or popover opened near the
   *  right/bottom edge must slide into view, not fall off screen. Shared by the context/table
   *  menu, the tracked-change popup, the comment popovers, and the input dialog. */
  clampPopup(el: HTMLElement): void {
    requestAnimationFrame(() => {
      const r = el.getBoundingClientRect();
      const vw = globalThis.innerWidth;
      const vh = globalThis.innerHeight;
      if (r.right > vw - 8) el.style.left = `${Math.max(8, vw - r.width - 8)}px`;
      if (r.bottom > vh - 8) el.style.top = `${Math.max(8, vh - r.height - 8)}px`;
      if (r.left < 8) el.style.left = '8px';
      if (r.top < 8) el.style.top = '8px';
    });
  }

  openMenu(
    x: number,
    y: number,
    items: Array<[string, (() => void) | null] | ScriptorContextMenuItem>,
  ): void {
    this.closeTableMenu();
    const norm: ScriptorContextMenuItem[] = items.map((it) =>
      Array.isArray(it) ? { label: it[0], onClick: it[1] ?? undefined } : it,
    );
    const menu = document.createElement('div');
    menu.className = 'scriptor-table-menu';
    menu.style.cssText =
      `position:fixed;left:${x}px;top:${y}px;z-index:2147483647;background:var(--scr-surface, #fff);` +
      'border:1px solid var(--scr-border-strong, #c8ccd4);border-radius:6px;box-shadow:0 6px 20px rgba(0,0,0,.18);' +
      'padding:4px;font:13px system-ui,-apple-system,sans-serif;min-width:184px;max-height:70vh;overflow-y:auto;';
    for (const item of norm) {
      const isSep =
        item.separator || (!item.onClick && (!item.label || /^[-—]$/.test(item.label.trim())));
      if (isSep) {
        const hr = document.createElement('div');
        hr.style.cssText = 'height:1px;background:#e6e8ee;margin:4px 2px;';
        menu.append(hr);
        continue;
      }
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = item.label ?? '';
      const enabled = !item.disabled && !!item.onClick;
      b.disabled = !enabled;
      b.style.cssText =
        'display:block;width:100%;text-align:left;border:0;background:none;padding:6px 10px;' +
        `border-radius:4px;cursor:${enabled ? 'pointer' : 'default'};font:inherit;` +
        `color:${enabled ? 'var(--scr-ink, #1a1a1a)' : 'var(--scr-text-faint, #9aa3b2)'};`;
      if (enabled) {
        b.addEventListener('mouseenter', () => {
          b.style.background = 'var(--scr-hover, #eef1f7)';
        });
        b.addEventListener('mouseleave', () => {
          b.style.background = 'none';
        });
        b.addEventListener('click', () => {
          item.onClick?.();
          this.closeTableMenu();
        });
      }
      menu.append(b);
    }
    document.body.append(menu);
    this.tableMenuEl = menu;
    this.clampPopup(menu);
    // Dismiss on an outside click or Escape (registered next tick so this event doesn't close it).
    this.onDocPointer = (ev: Event) => {
      if (ev instanceof KeyboardEvent) {
        if (ev.key === 'Escape') this.closeTableMenu();
        return;
      }
      if (this.tableMenuEl && !this.tableMenuEl.contains(ev.target as Node)) this.closeTableMenu();
    };
    setTimeout(() => {
      if (!this.onDocPointer) return;
      document.addEventListener('mousedown', this.onDocPointer, true);
      document.addEventListener('keydown', this.onDocPointer, true);
    }, 0);
  }

  closeTableMenu(): void {
    if (this.tableMenuEl) {
      this.tableMenuEl.remove();
      this.tableMenuEl = null;
    }
    if (this.onDocPointer) {
      document.removeEventListener('mousedown', this.onDocPointer, true);
      document.removeEventListener('keydown', this.onDocPointer, true);
      this.onDocPointer = null;
    }
  }

  showTrackTip(author: string, rest: string, x: number, y: number): void {
    if (!this.trackTip) {
      const t = document.createElement('div');
      t.className = 'scriptor-track-tip';
      t.style.cssText =
        'position:fixed;z-index:2147483647;pointer-events:none;background:#2a2f3a;color:var(--scr-surface, #fff);' +
        'font:12px system-ui,-apple-system,sans-serif;padding:4px 8px;border-radius:4px;' +
        'max-width:300px;box-shadow:0 4px 14px rgba(0,0,0,.22);display:flex;align-items:center;gap:6px;';
      document.body.append(t);
      this.trackTip = t;
    }
    const tip = this.trackTip;
    tip.replaceChildren();
    const avatar = this.h.authorAvatars.get(author);
    if (avatar) {
      const img = document.createElement('img');
      img.src = avatar;
      img.alt = '';
      img.style.cssText =
        'width:16px;height:16px;border-radius:50%;object-fit:cover;flex:0 0 auto;';
      tip.append(img);
    }
    const span = document.createElement('span');
    span.textContent = `${author} ${rest}`;
    tip.append(span);
    this.positionTip(x, y);
  }

  positionTip(x: number, y: number): void {
    if (!this.trackTip) return;
    this.trackTip.style.left = `${x + 12}px`;
    this.trackTip.style.top = `${y + 16}px`;
    // Keep the tooltip on screen while hovering near the right/bottom edges: pull it left of the
    // viewport edge, and flip it ABOVE the cursor when there's no room below (synchronous - it
    // trails every mouse move, so a next-frame clamp would visibly lag).
    const r = this.trackTip.getBoundingClientRect();
    const vw = globalThis.innerWidth;
    const vh = globalThis.innerHeight;
    if (r.right > vw - 8) this.trackTip.style.left = `${Math.max(8, vw - r.width - 8)}px`;
    if (r.bottom > vh - 8) this.trackTip.style.top = `${Math.max(8, y - r.height - 10)}px`;
  }

  hideTrackTip(): void {
    if (this.trackTip) {
      this.trackTip.remove();
      this.trackTip = null;
    }
    this.h.lastHoverKey = '';
  }

  showTrackPopup(
    x: number,
    y: number,
    para: number,
    hit: NonNullable<ReturnType<ScriptorDoc['trackAt']>>,
  ): void {
    this.closeTrackPopup();
    const id = hit.id;
    const noun = TRACK_NOUN[hit.kind] ?? 'Change';
    const verb = TRACK_VERB[hit.kind] ?? 'changed';
    const menu = document.createElement('div');
    menu.className = 'scriptor-track-popup';
    menu.style.cssText =
      `position:fixed;left:${x}px;top:${y}px;z-index:2147483647;background:var(--scr-surface, #fff);` +
      'border:1px solid var(--scr-border-strong, #c8ccd4);border-radius:6px;box-shadow:0 6px 20px rgba(0,0,0,.18);' +
      'padding:4px;font:13px system-ui,-apple-system,sans-serif;min-width:184px;max-width:280px;';

    const head = document.createElement('div');
    head.textContent = `${this.h.displayAuthor(hit.author) || 'Unknown'} ${verb}`;
    head.style.cssText =
      'padding:5px 10px 6px;color:var(--scr-text-muted, #46506a);font-size:12px;font-weight:600;';
    menu.append(head);

    const mkBtn = (label: string, fn: () => void): void => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = label;
      b.style.cssText =
        'display:block;width:100%;text-align:left;border:0;background:none;padding:6px 10px;' +
        'border-radius:4px;cursor:pointer;color:var(--scr-ink, #1a1a1a);font:inherit;';
      b.addEventListener('mouseenter', () => {
        b.style.background = 'var(--scr-hover, #eef1f7)';
      });
      b.addEventListener('mouseleave', () => {
        b.style.background = 'none';
      });
      b.addEventListener('click', () => {
        fn();
        this.closeTrackPopup();
      });
      menu.append(b);
    };
    mkBtn(`Accept ${noun}`, () => this.h.resolveRevision(para, id, true));
    mkBtn(`Reject ${noun}`, () => this.h.resolveRevision(para, id, false));

    document.body.append(menu);
    this.trackPopupEl = menu;
    this.clampPopup(menu);
    const onDoc = (ev: Event): void => {
      if (ev instanceof KeyboardEvent) {
        if (ev.key === 'Escape') this.closeTrackPopup();
        return;
      }
      if (this.trackPopupEl && !this.trackPopupEl.contains(ev.target as Node))
        this.closeTrackPopup();
    };
    // A fixed popup can't follow its anchor through a scroll, so dismiss on scroll/resize (capture
    // catches the page stage's own scroll, which doesn't bubble) - same as the ribbon flyout.
    const onReflow = (): void => this.closeTrackPopup();
    setTimeout(() => {
      document.addEventListener('mousedown', onDoc, true);
      document.addEventListener('keydown', onDoc, true);
      window.addEventListener('scroll', onReflow, true);
      window.addEventListener('resize', onReflow);
      this.trackPopupCleanup = () => {
        document.removeEventListener('mousedown', onDoc, true);
        document.removeEventListener('keydown', onDoc, true);
        window.removeEventListener('scroll', onReflow, true);
        window.removeEventListener('resize', onReflow);
      };
    }, 0);
  }

  closeTrackPopup(): void {
    if (this.trackPopupEl) {
      this.trackPopupEl.remove();
      this.trackPopupEl = null;
    }
    if (this.trackPopupCleanup) {
      this.trackPopupCleanup();
      this.trackPopupCleanup = null;
    }
  }

  /** Create + mount the shared comment popover shell at `(x, y)` (fixed, body-mounted, dismiss on
   *  outside-click / Esc, clamped into the viewport). Returns the element to fill in. */
  mountCommentPopup(x: number, y: number): HTMLDivElement {
    this.closeCommentPopup();
    const menu = document.createElement('div');
    menu.className = 'scriptor-comment-popup';
    menu.style.cssText =
      `position:fixed;left:${x}px;top:${y}px;z-index:2147483647;background:var(--scr-surface, #fff);` +
      'border:1px solid var(--scr-border-strong, #c8ccd4);border-radius:8px;box-shadow:0 6px 24px rgba(0,0,0,.20);' +
      'padding:8px;font:13px system-ui,-apple-system,sans-serif;width:280px;max-height:60vh;overflow:auto;';
    document.body.append(menu);
    this.commentPopupEl = menu;
    const onDoc = (ev: Event): void => {
      if (ev instanceof KeyboardEvent) {
        if (ev.key === 'Escape') this.closeCommentPopup();
        return;
      }
      if (this.commentPopupEl && !this.commentPopupEl.contains(ev.target as Node)) {
        this.closeCommentPopup();
      }
    };
    // A fixed popup can't follow its anchor through a scroll, so dismiss on scroll/resize (capture
    // catches the page stage's own scroll, which doesn't bubble) - same as the ribbon flyout.
    const onReflow = (): void => this.closeCommentPopup();
    setTimeout(() => {
      document.addEventListener('mousedown', onDoc, true);
      document.addEventListener('keydown', onDoc, true);
      window.addEventListener('scroll', onReflow, true);
      window.addEventListener('resize', onReflow);
      this.commentPopupCleanup = () => {
        document.removeEventListener('mousedown', onDoc, true);
        document.removeEventListener('keydown', onDoc, true);
        window.removeEventListener('scroll', onReflow, true);
        window.removeEventListener('resize', onReflow);
      };
    }, 0);
    this.clampPopup(menu);
    return menu;
  }

  closeCommentPopup(): void {
    if (this.commentPopupEl) {
      this.commentPopupEl.remove();
      this.commentPopupEl = null;
    }
    if (this.commentPopupCleanup) {
      this.commentPopupCleanup();
      this.commentPopupCleanup = null;
    }
  }

  popupButton(label: string, primary: boolean): HTMLButtonElement {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.style.cssText = primary
      ? 'border:0;background:var(--scr-accent, #2563eb);color:var(--scr-surface, #fff);padding:5px 12px;border-radius:6px;cursor:pointer;font:inherit;'
      : 'border:1px solid var(--scr-border-strong, #c8ccd4);background:var(--scr-surface, #fff);color:var(--scr-ink, #1a1a1a);padding:5px 12px;border-radius:6px;cursor:pointer;font:inherit;';
    return b;
  }

  /** A styled single-line input dialog anchored at the caret - the reusable replacement for
   *  `window.prompt`, sharing the comment-popover chrome so bookmarks + hyperlinks look the same. Mounts
   *  on `<body>`, clamps into the viewport, and resolves the (untrimmed) field value on Enter / OK, or
   *  `null` on Esc / Cancel / outside-click. Restores editor focus on close. Never rejects. */
  promptInput(opts: InputDialogOptions): Promise<string | null> {
    return new Promise((resolve) => {
      const { x, y } = this.h.sel
        ? this.h.caretClientXY(this.h.sel.focus.para, this.h.sel.focus.off)
        : { x: 80, y: 80 };
      const menu = document.createElement('div');
      menu.className = 'scriptor-input-dialog';
      menu.style.cssText =
        `position:fixed;left:${x}px;top:${y}px;z-index:2147483647;background:var(--scr-surface, #fff);` +
        'border:1px solid var(--scr-border-strong, #c8ccd4);border-radius:8px;box-shadow:0 6px 24px rgba(0,0,0,.20);' +
        'padding:10px;font:13px system-ui,-apple-system,sans-serif;width:300px;';
      document.body.append(menu);

      const head = document.createElement('div');
      head.textContent = opts.title;
      head.style.cssText =
        'font-weight:600;color:var(--scr-text-muted, #46506a);font-size:12px;margin:0 2px 7px;';
      menu.append(head);

      const input = document.createElement('input');
      input.type = 'text';
      input.value = opts.value ?? '';
      if (opts.placeholder) input.placeholder = opts.placeholder;
      input.style.cssText =
        'width:100%;box-sizing:border-box;border:1px solid var(--scr-border-strong, #c8ccd4);border-radius:6px;' +
        'padding:6px 8px;font:inherit;';
      menu.append(input);

      if (opts.hint) {
        const hint = document.createElement('div');
        hint.textContent = opts.hint;
        hint.style.cssText =
          'color:var(--scr-text-subtle, #8893ab);font-size:11px;margin:6px 2px 0;line-height:1.35;';
        menu.append(hint);
      }

      const row = document.createElement('div');
      row.style.cssText = 'display:flex;justify-content:flex-end;gap:6px;margin-top:9px;';
      const cancel = this.popupButton('Cancel', false);
      const ok = this.popupButton(opts.okLabel ?? 'OK', true);
      row.append(cancel, ok);
      menu.append(row);

      let done = false;
      const close = (value: string | null): void => {
        if (done) return;
        done = true;
        menu.remove();
        document.removeEventListener('mousedown', onDoc, true);
        document.removeEventListener('keydown', onKey, true);
        this.h.keys.focus({ preventScroll: true });
        resolve(value);
      };
      const onDoc = (ev: MouseEvent): void => {
        if (!menu.contains(ev.target as Node)) close(null);
      };
      const onKey = (ev: KeyboardEvent): void => {
        if (ev.key === 'Escape') {
          ev.preventDefault();
          close(null);
        }
      };
      cancel.addEventListener('click', () => close(null));
      ok.addEventListener('click', () => close(input.value));
      input.addEventListener('keydown', (ev) => {
        if (ev.key === 'Enter') {
          ev.preventDefault();
          close(input.value);
        }
      });

      setTimeout(() => {
        document.addEventListener('mousedown', onDoc, true);
        document.addEventListener('keydown', onKey, true);
        input.focus();
        input.select();
      }, 0);

      this.clampPopup(menu);
    });
  }

  /** A composer for a brand-new comment over `anchor..focus` (resolved by [`newComment`]). */
  showCommentComposer(x: number, y: number, anchor: CaretPos, focus: CaretPos): void {
    const menu = this.mountCommentPopup(x, y);

    const head = document.createElement('div');
    head.textContent = 'New comment';
    head.style.cssText =
      'font-weight:600;color:var(--scr-text-muted, #46506a);font-size:12px;margin:0 2px 6px;';
    menu.append(head);

    const ta = document.createElement('textarea');
    ta.placeholder = 'Type a comment…';
    ta.style.cssText =
      'width:100%;box-sizing:border-box;min-height:56px;resize:vertical;border:1px solid var(--scr-border-strong, #c8ccd4);' +
      'border-radius:6px;padding:6px 8px;font:inherit;';
    menu.append(ta);

    const row = document.createElement('div');
    row.style.cssText = 'display:flex;justify-content:flex-end;gap:6px;margin-top:6px;';
    const cancel = this.popupButton('Cancel', false);
    const add = this.popupButton('Comment', true);
    row.append(cancel, add);
    menu.append(row);

    cancel.addEventListener('click', () => this.closeCommentPopup());
    const submit = (): void => {
      const text = ta.value.trim();
      if (!text) {
        this.closeCommentPopup();
        return;
      }
      try {
        this.h.doc.addComment(anchor.para, anchor.off, focus.para, focus.off, text);
        this.closeCommentPopup();
        this.h.afterResolve();
      } catch (err) {
        console.error('scriptor add-comment error', err);
      }
    };
    add.addEventListener('click', submit);
    ta.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') submit();
    });
    setTimeout(() => ta.focus(), 0);
  }

  /** The thread popover for the comment(s) anchored at the caret: every comment + replies, plus a
   *  reply box and resolve / delete actions on the root. */
  showCommentThread(x: number, y: number, ids: number[]): void {
    let all: CommentJson[];
    try {
      all = JSON.parse(this.h.doc.listComments()) as CommentJson[];
    } catch {
      return;
    }
    // The thread = the anchored ids + every (transitive) reply.
    const inThread = new Set<number>(ids);
    for (let grew = true; grew; ) {
      grew = false;
      for (const c of all) {
        if (c.parent != null && inThread.has(c.parent) && !inThread.has(c.id)) {
          inThread.add(c.id);
          grew = true;
        }
      }
    }
    const thread = all.filter((c) => inThread.has(c.id)).sort((a, b) => a.id - b.id);
    if (!thread.length) return;
    const root = thread.find((c) => c.parent == null) ?? thread[0]!;
    const menu = this.mountCommentPopup(x, y);

    for (const c of thread) menu.append(this.renderCommentItem(c, root));

    const ta = document.createElement('textarea');
    ta.placeholder = 'Reply…';
    ta.style.cssText =
      'width:100%;box-sizing:border-box;min-height:40px;resize:vertical;border:1px solid var(--scr-border-strong, #c8ccd4);' +
      'border-radius:6px;padding:6px 8px;font:inherit;margin-top:4px;';
    menu.append(ta);

    const row = document.createElement('div');
    row.style.cssText =
      'display:flex;justify-content:space-between;align-items:center;margin-top:6px;';
    const left = document.createElement('div');
    left.style.cssText = 'display:flex;gap:6px;';
    const resolveBtn = this.popupButton(root.resolved ? 'Reopen' : 'Resolve', false);
    const deleteBtn = this.popupButton('Delete', false);
    deleteBtn.style.color = 'var(--scr-danger, #b42318)';
    left.append(resolveBtn, deleteBtn);
    const reply = this.popupButton('Reply', true);
    row.append(left, reply);
    menu.append(row);

    resolveBtn.addEventListener('click', () => {
      try {
        this.h.doc.resolveComment(root.id, !root.resolved);
        this.closeCommentPopup();
        this.h.afterResolve();
      } catch (err) {
        console.error('scriptor resolve-comment error', err);
      }
    });
    deleteBtn.addEventListener('click', () => {
      try {
        this.h.doc.deleteComment(root.id);
        this.closeCommentPopup();
        this.h.afterResolve();
      } catch (err) {
        console.error('scriptor delete-comment error', err);
      }
    });
    const submitReply = (): void => {
      const text = ta.value.trim();
      if (!text) return;
      try {
        this.h.doc.replyComment(root.id, text);
        this.closeCommentPopup();
        this.h.afterResolve();
      } catch (err) {
        console.error('scriptor reply-comment error', err);
      }
    };
    reply.addEventListener('click', submitReply);
    ta.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') submitReply();
    });
  }

  renderCommentItem(c: CommentJson, root: CommentJson): HTMLDivElement {
    const item = document.createElement('div');
    const isReply = c.parent != null;
    item.style.cssText =
      `padding:6px 8px;border-radius:6px;margin-bottom:4px;background:${c.resolved ? '#f1f5f9' : '#fbf6ec'};` +
      (isReply ? 'margin-left:14px;' : '');
    const head = document.createElement('div');
    head.style.cssText =
      'display:flex;justify-content:space-between;font-size:11px;color:#5a6172;margin-bottom:2px;';
    const who = document.createElement('span');
    who.textContent = this.h.displayAuthor(c.author) || 'Unknown';
    who.style.fontWeight = '600';
    if (root.resolved && c.id === root.id) {
      const tag = document.createElement('span');
      tag.textContent = ' ✓ resolved';
      tag.style.cssText = 'color:#0a7d33;font-weight:400;';
      who.append(tag);
    }
    const when = document.createElement('span');
    when.textContent = formatCommentDate(c.date);
    head.append(who, when);
    const body = document.createElement('div');
    body.textContent = c.text;
    body.style.cssText = 'white-space:pre-wrap;color:var(--scr-ink, #1a1a1a);';
    item.append(head, body);
    return item;
  }
}
