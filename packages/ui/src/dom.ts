// Shared DOM helpers + the chrome stylesheet, used by the ribbon, rulers, status bar, and workspace.

import { type IconName, icon } from './icons';

export { type IconName, icon } from './icons';

export function h(tag: string, props: Record<string, string> = {}): HTMLElement {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === 'class') el.className = v;
    else el.setAttribute(k, v);
  }
  return el;
}

export function opt(value: string, label: string): HTMLOptionElement {
  const o = document.createElement('option');
  o.value = value;
  o.textContent = label;
  return o;
}

/** A text-glyph button (used for the lettered B / I / U / S, which are conventionally letters). */
export function iconBtn(
  glyph: string,
  title: string,
  cls: string,
  onClick: () => void,
): HTMLButtonElement {
  const btn = h('button', {
    class: `scr-btn ${cls}`.trim(),
    type: 'button',
    title,
  }) as HTMLButtonElement;
  btn.textContent = glyph;
  btn.addEventListener('click', onClick);
  return btn;
}

/** A Phosphor-icon button (icon only). `opts.rotate` reuses one glyph for two orientations. */
export function svgBtn(
  name: IconName,
  title: string,
  cls: string,
  onClick: () => void,
  opts: { rotate?: number; size?: number; label?: string } = {},
): HTMLButtonElement {
  const btn = h('button', {
    class: `scr-btn ${cls}`.trim(),
    type: 'button',
    title,
  }) as HTMLButtonElement;
  btn.append(icon(name, opts.size ?? 18, opts.rotate ?? 0));
  if (opts.label) {
    btn.classList.add('scr-btn-labeled'); // left-align icon + label so stacked buttons grid up
    btn.append(labelSpan(opts.label));
  }
  btn.addEventListener('click', onClick);
  return btn;
}

function labelSpan(text: string): HTMLElement {
  const s = h('span', { class: 'scr-btn-label' });
  s.textContent = text;
  return s;
}

/** One entry in a split button's dropdown menu. */
export interface SplitItem {
  label: string;
  icon?: IconName;
  onClick: () => void;
}

/**
 * A Word-style split button: a primary icon+label action plus a caret that drops a small menu of
 * related actions (e.g. Accept / Accept All). The menu dismisses on outside click, Esc, or after a
 * selection - reusing the same lightweight pattern as the editor's inline popups.
 */
export function splitBtn(
  name: IconName,
  label: string,
  title: string,
  primary: () => void,
  items: SplitItem[],
): HTMLElement {
  const wrap = h('div', { class: 'scr-split' });
  const main = h('button', {
    class: 'scr-btn scr-split-main',
    type: 'button',
    title,
  }) as HTMLButtonElement;
  main.append(icon(name, 18), labelSpan(label));
  main.addEventListener('click', primary);
  const caret = h('button', {
    class: 'scr-btn scr-split-caret',
    type: 'button',
    title: `${label} options`,
  }) as HTMLButtonElement;
  caret.append(icon('caretDown', 14));
  wrap.append(main, caret);

  let menu: HTMLElement | null = null;
  let onDoc: ((e: Event) => void) | null = null;
  let onReflow: (() => void) | null = null;
  const close = (): void => {
    if (menu) {
      menu.remove();
      menu = null;
    }
    if (onDoc) {
      document.removeEventListener('mousedown', onDoc, true);
      document.removeEventListener('keydown', onDoc, true);
      onDoc = null;
    }
    if (onReflow) {
      window.removeEventListener('scroll', onReflow, true);
      window.removeEventListener('resize', onReflow);
      onReflow = null;
    }
  };
  const open = (): void => {
    if (menu) {
      close();
      return;
    }
    // The menu mounts on <body> as position:fixed so the ribbon's `overflow-x:auto` can't clip it
    // (an absolutely-positioned child would get trapped inside the scrolling panel).
    const m = h('div', { class: 'scr-menu' });
    for (const it of items) {
      const b = h('button', { class: 'scr-menu-item', type: 'button' }) as HTMLButtonElement;
      if (it.icon) b.append(icon(it.icon, 16));
      b.append(labelSpan(it.label));
      b.addEventListener('click', () => {
        close();
        it.onClick();
      });
      m.append(b);
    }
    document.body.append(m);
    menu = m;
    // Drop it just under the split button, clamped into the viewport.
    const r = wrap.getBoundingClientRect();
    const left = Math.max(8, Math.min(r.left, window.innerWidth - m.offsetWidth - 8));
    m.style.left = `${Math.round(left)}px`;
    m.style.top = `${Math.round(r.bottom + 3)}px`;
    onDoc = (e: Event): void => {
      if (e instanceof KeyboardEvent) {
        if (e.key === 'Escape') close();
        return;
      }
      const t = e.target as Node;
      if (!m.contains(t) && !wrap.contains(t)) close();
    };
    onReflow = () => close(); // a fixed menu can't follow the button, so dismiss on scroll/resize
    setTimeout(() => {
      if (!onDoc) return;
      document.addEventListener('mousedown', onDoc, true);
      document.addEventListener('keydown', onDoc, true);
      window.addEventListener('scroll', onReflow as () => void, true);
      window.addEventListener('resize', onReflow as () => void);
    }, 0);
  };
  caret.addEventListener('click', (e) => {
    e.stopPropagation();
    open();
  });
  return wrap;
}

/**
 * Mount `content` as a fixed-position popup just under `anchor`, clamped into the viewport, with the
 * same dismiss behaviour as the split-button menu (outside mousedown, Esc, scroll/resize). Mounts on
 * <body> so the ribbon's `overflow-x:auto` can't clip it. Returns a `close` fn (idempotent).
 */
export function openFlyout(
  anchor: HTMLElement,
  content: HTMLElement,
  onClose?: () => void,
): () => void {
  document.body.append(content);
  const r = anchor.getBoundingClientRect();
  const left = Math.max(8, Math.min(r.left, window.innerWidth - content.offsetWidth - 8));
  content.style.left = `${Math.round(left)}px`;
  // Below the anchor by default; when there's no room (a dropdown near the bottom of a short
  // window), flip ABOVE the anchor if it fits there, else pin to the bottom viewport inset - a
  // flyout must never run off screen.
  const h = content.offsetHeight;
  let top = r.bottom + 3;
  if (top + h > window.innerHeight - 8) {
    top = r.top - 3 - h >= 8 ? r.top - 3 - h : Math.max(8, window.innerHeight - h - 8);
  }
  content.style.top = `${Math.round(top)}px`;
  let closed = false;
  const close = (): void => {
    if (closed) return;
    closed = true;
    content.remove();
    document.removeEventListener('mousedown', onDoc, true);
    document.removeEventListener('keydown', onDoc, true);
    window.removeEventListener('scroll', onReflow, true);
    window.removeEventListener('resize', onReflow);
    onClose?.(); // let the opener clear any toggle handle, however we were dismissed
  };
  const onDoc = (e: Event): void => {
    if (e instanceof KeyboardEvent) {
      if (e.key === 'Escape') close();
      return;
    }
    const t = e.target as Node;
    if (!content.contains(t) && !anchor.contains(t)) close();
  };
  const onReflow = (): void => close(); // a fixed popup can't follow the anchor, so dismiss on scroll/resize
  setTimeout(() => {
    if (closed) return;
    document.addEventListener('mousedown', onDoc, true);
    document.addEventListener('keydown', onDoc, true);
    window.addEventListener('scroll', onReflow, true);
    window.addEventListener('resize', onReflow);
  }, 0);
  return close;
}

/** One choice in a [`dropdown`]. */
export interface DropdownItem {
  value: string;
  label: string;
  /** Optional CSS `font-family` to render this item's label in (the font dropdown previews each entry
   *  in its own typeface). Applied to the menu item and, when selected, the trigger label. */
  font?: string;
}

/** Handle for a [`dropdown`]: the element to mount plus imperative value / option updates. */
export interface DropdownHandle {
  el: HTMLElement;
  /** Reflect the current value - shows the matching item's label, or the raw value if none matches. */
  setValue(value: string): void;
  /** Replace the option list (e.g. when the measurement unit changes). */
  setItems(items: DropdownItem[]): void;
}

/**
 * A styled replacement for a native `<select>`, built from the same flyout + menu chrome as the split
 * buttons and numbering picker - so every ribbon dropdown matches the editor's look instead of the OS.
 * With `editable`, the trigger is a typable combo-box (Word's font-size box): pick a value or type one.
 */
export function dropdown(o: {
  items: DropdownItem[];
  value?: string;
  title?: string;
  className?: string;
  width?: string;
  editable?: boolean;
  onChange: (value: string) => void;
}): DropdownHandle {
  let items = o.items;
  let menuClose: (() => void) | null = null;
  let last = o.value ?? '';

  const el = h('div', { class: `scr-dd ${o.className ?? ''}`.trim() });
  if (o.title) el.setAttribute('title', o.title);
  if (o.width) el.style.width = o.width;

  const input = o.editable
    ? (h('input', { class: 'scr-dd-input', type: 'text' }) as HTMLInputElement)
    : null;
  const label = input ? null : h('span', { class: 'scr-dd-label' });
  if (input) {
    el.classList.add('scr-combo');
    el.append(input);
  } else {
    el.append(label!);
  }
  const caret = h('span', { class: 'scr-dd-caret' });
  caret.append(icon('caretDown', 12));
  el.append(caret);

  const labelFor = (value: string): string =>
    items.find((it) => it.value === value)?.label ?? value;
  const setDisplay = (value: string): void => {
    if (input) input.value = value;
    else {
      label!.textContent = labelFor(value);
      // Preview the selected value in its own typeface (the font dropdown), else clear any prior font.
      label!.style.fontFamily = items.find((it) => it.value === value)?.font ?? '';
    }
  };
  if (o.value !== undefined) setDisplay(o.value);

  const commit = (value: string): void => {
    setDisplay(value);
    if (value === last) return; // a re-pick / Enter-then-blur shouldn't fire twice
    last = value;
    o.onChange(value);
  };

  const openMenu = (): void => {
    if (menuClose) {
      menuClose(); // re-click toggles it shut
      return;
    }
    const menu = h('div', { class: 'scr-menu scr-dd-menu' });
    const cur = input ? input.value : (label!.textContent ?? '');
    for (const it of items) {
      const item = h('button', { class: 'scr-menu-item', type: 'button' }) as HTMLButtonElement;
      item.textContent = it.label;
      if (it.font) item.style.fontFamily = it.font; // preview the entry in its own typeface
      if (it.label === cur || it.value === cur) item.classList.add('active');
      item.addEventListener('click', () => {
        menuClose?.();
        commit(it.value);
      });
      menu.append(item);
    }
    menuClose = openFlyout(el, menu, () => {
      menuClose = null;
    });
  };

  caret.addEventListener('click', (e) => {
    e.stopPropagation();
    openMenu();
  });
  if (input) {
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        commit(input.value.trim());
        input.blur();
      }
    });
    input.addEventListener('change', () => commit(input.value.trim()));
  } else {
    el.addEventListener('click', openMenu);
  }

  return {
    el,
    setValue: (v) => {
      last = v;
      setDisplay(v);
    },
    setItems: (next) => {
      items = next;
    },
  };
}

/**
 * Word-style responsive groups: when a panel's groups don't fit its width, the trailing ones collapse
 * behind a single "More" button that opens them in a flyout - so the ribbon never grows taller or
 * scrolls. Re-evaluated on resize / tab-show via a ResizeObserver. The overflow set is always the
 * trailing, contiguous block, so moving it into the flyout and restoring it before "More" preserves
 * order. No-op where ResizeObserver is unavailable (the panel just scrolls, at a fixed height).
 */
export function makeRibbonResponsive(panel: HTMLElement): void {
  if (typeof ResizeObserver === 'undefined') return;
  const more = h('button', {
    class: 'scr-btn scr-more',
    type: 'button',
    title: 'More commands',
  }) as HTMLButtonElement;
  more.append(icon('caretDown', 16));
  more.style.display = 'none';
  panel.append(more);

  const items = (): HTMLElement[] =>
    Array.from(panel.children).filter(
      (c): c is HTMLElement => c instanceof HTMLElement && c !== more,
    );

  let close: (() => void) | null = null;
  more.addEventListener('click', (e) => {
    e.stopPropagation();
    if (close) {
      close();
      return;
    }
    const hidden = items().filter((el) => el.style.display === 'none');
    if (!hidden.some((el) => el.classList.contains('scr-group'))) return;
    const menu = h('div', { class: 'scr-menu scr-more-menu' });
    for (const el of hidden) {
      el.style.display = el.classList.contains('scr-vsep') ? 'none' : ''; // show groups, drop dividers
      menu.append(el);
    }
    close = openFlyout(more, menu, () => {
      for (const el of hidden) {
        el.style.display = 'none';
        panel.insertBefore(el, more); // restore the trailing block in order, before "More"
      }
      close = null;
    });
  });

  const reflow = (): void => {
    if (close) return; // don't reshuffle while the flyout owns the overflow groups
    const all = items();
    for (const el of all) el.style.display = '';
    more.style.display = 'none';
    if (panel.scrollWidth <= panel.clientWidth + 1) return;
    more.style.display = '';
    for (let i = all.length - 1; i >= 0 && panel.scrollWidth > panel.clientWidth + 1; i--) {
      all[i]!.style.display = 'none';
    }
  };
  new ResizeObserver(() => reflow()).observe(panel);
}

export function toggle(btn: HTMLButtonElement, on: boolean): void {
  btn.classList.toggle('active', on);
}

export function row(children: HTMLElement[]): HTMLElement {
  const r = h('div', { class: 'scr-row' });
  r.append(...children);
  return r;
}

/** A vertical stack of controls (e.g. Cut over Copy beside a tall Paste button). */
export function col(children: HTMLElement[]): HTMLElement {
  const c = h('div', { class: 'scr-col' });
  c.append(...children);
  return c;
}

export function group(label: string, rows: HTMLElement[]): HTMLElement {
  const g = h('div', { class: 'scr-group' });
  const body = h('div', { class: 'scr-group-body' });
  body.append(...rows);
  g.append(body);
  if (label) {
    const lab = h('div', { class: 'scr-group-label' });
    lab.textContent = label;
    g.append(lab);
  }
  return g;
}

export function labeled(label: string, control: HTMLElement): HTMLElement {
  const wrap = h('label', { class: 'scr-labeled' });
  const span = h('span', { class: 'scr-labeled-text' });
  span.textContent = label;
  wrap.append(span, control);
  return wrap;
}

export function sep(): HTMLElement {
  return h('div', { class: 'scr-vsep' });
}

let stylesInjected = false;
export function injectStyles(): void {
  if (stylesInjected || typeof document === 'undefined') return;
  stylesInjected = true;
  const style = document.createElement('style');
  style.textContent = CHROME_CSS;
  document.head.append(style);
}

const CHROME_CSS = `
/* Design tokens. Integrators retheme by overriding these on the host (or :root) - no !important, no
   fork. Defined on :root so they also reach the body-mounted popovers (menus, modals, the in-canvas
   comment/track popovers in scriptor-core, which read the same vars with literal fallbacks). A dark
   theme is one override block: set [data-scr-theme="dark"] { --scr-surface: #1e1e1e; ... }. */
:root {
  --scr-font: system-ui, -apple-system, sans-serif;
  --scr-ribbon-h: 96px;          /* ribbon panel height (Fluent ~92-96px; floor, never clips) */
  --scr-ctl-h: 24px;             /* small-control height (buttons, dropdowns, inputs) - Fluent rhythm */
  --scr-radius-sm: 4px;          /* control corner radius */
  --scr-surface: #ffffff;        /* panels, menus, cards, inputs */
  --scr-bg: #f3f5f9;             /* ribbon / status / ruler chrome */
  --scr-bg-stage: #eef0f5;       /* page-stage backdrop */
  --scr-text: #1f2430;           /* primary text */
  --scr-text-muted: #46506a;     /* labels, secondary text */
  --scr-text-subtle: #8893ab;    /* placeholders, muted captions */
  --scr-text-faint: #aab2c4;     /* disabled tabs / filtered-out */
  --scr-text-disabled: #c2c8d4;  /* disabled buttons */
  --scr-ink: #1a1a1a;            /* dense body text in popovers / inputs */
  --scr-border: #dde2ec;         /* control borders */
  --scr-border-strong: #c8ccd4;  /* menu / popup borders */
  --scr-border-subtle: #e2e6ee;  /* separators, light dividers */
  --scr-border-chrome: #d7dce6;  /* ribbon / status frame */
  --scr-hover: #eef1f6;          /* hover fill */
  --scr-hover-strong: #e7ebf3;   /* tab hover, count badge */
  --scr-accent: #2563eb;         /* primary action */
  --scr-accent-bg: #dbe7ff;      /* active / selected fill */
  --scr-accent-border: #9cc0ff;  /* active border */
  --scr-accent-text: #1f4d78;    /* active label */
  --scr-danger: #b42318;
  --scr-danger-bg: #fde8e6;
  --scr-danger-border: #f3c0ba;
  --scr-shadow-menu: 0 6px 20px rgba(20, 28, 48, 0.18);
  --scr-shadow-modal: 0 12px 40px rgba(20, 28, 48, 0.28);
}
/* Opt-in dark theme: set data-scr-theme="dark" on <html> or <body> (a shared ancestor of both the
   workspace and the body-mounted popovers) and every token flips. Integrators can copy this block and
   tune it, or override individual --scr-* vars on their own wrapper. */
[data-scr-theme="dark"] {
  --scr-surface: #1e1e22;
  --scr-bg: #26262b;
  --scr-bg-stage: #15151a;
  --scr-text: #e6e8ee;
  --scr-text-muted: #b2b8c6;
  --scr-text-subtle: #8a90a0;
  --scr-text-faint: #6b7180;
  --scr-text-disabled: #565b68;
  --scr-ink: #e6e8ee;
  --scr-border: #3a3a42;
  --scr-border-strong: #4a4a54;
  --scr-border-subtle: #303036;
  --scr-border-chrome: #3a3a42;
  --scr-hover: #2e2e35;
  --scr-hover-strong: #34343c;
  --scr-accent: #4d8dff;
  --scr-accent-bg: #1f3a5f;
  --scr-accent-border: #3a6db5;
  --scr-accent-text: #cfe0ff;
  --scr-danger: #ff6b5e;
  --scr-danger-bg: #3a1f1d;
  --scr-danger-border: #5e2f2a;
  --scr-shadow-menu: 0 6px 20px rgba(0, 0, 0, 0.5);
  --scr-shadow-modal: 0 12px 40px rgba(0, 0, 0, 0.6);
}
.scr-ribbon { font-family: var(--scr-font); font-size: 13px; color: var(--scr-text);
  background: var(--scr-bg); border-bottom: 1px solid var(--scr-border-chrome); user-select: none; }
.scr-tabs { display: flex; align-items: center; gap: 2px; padding: 4px 8px 0; }
.scr-qat { display: flex; gap: 2px; margin-right: 6px; padding-right: 6px; border-right: 1px solid var(--scr-border-chrome);
  align-self: center; }
.scr-btn:disabled { color: var(--scr-text-disabled); cursor: default; background: transparent; border-color: transparent; }
.scr-tab { font: inherit; border: none; background: transparent; padding: 5px 12px; border-radius: 6px 6px 0 0;
  cursor: pointer; color: var(--scr-text-muted); }
.scr-tab:hover { background: var(--scr-hover-strong); }
.scr-tab.active { background: var(--scr-surface); color: var(--scr-text); font-weight: 600; box-shadow: 0 -1px 0 var(--scr-border-chrome) inset; }
.scr-tab-disabled { color: var(--scr-text-faint); cursor: default; }
.scr-panels { background: var(--scr-surface); }
/* Fixed-height ribbon: a panel is always --scr-ribbon-h tall regardless of tab or window width, so the
   chrome never reflows vertically. When groups don't fit the width, overflow scrolls / collapses
   horizontally - the height stays put. */
.scr-panel { display: flex; align-items: stretch; gap: 0; padding: 4px 6px; min-height: var(--scr-ribbon-h);
  box-sizing: border-box; overflow: visible; }
.scr-panel.scr-hidden { display: none; }
/* Every group is a fixed-height column: a flexible body (controls) over a fixed label row. Because all
   groups share the panel height, their bodies + labels line up on a common baseline grid. */
.scr-group { display: flex; flex-direction: column; padding: 0 7px; flex: 0 0 auto; min-width: 0; }
.scr-group-body { display: flex; flex-direction: column; justify-content: center; gap: 4px; flex: 1;
  min-height: 0; }
.scr-group-label { text-align: center; color: var(--scr-text-subtle); font-size: 11px; line-height: 1;
  letter-spacing: 0.02em; padding: 5px 0 3px; flex: 0 0 auto; white-space: nowrap; }
.scr-row { display: flex; align-items: center; gap: 2px; }
.scr-col { display: flex; flex-direction: column; gap: 2px; }
.scr-vsep { align-self: center; width: 1px; height: 62px; background: var(--scr-border-subtle); margin: 0 4px; }
/* Overflow "More" button: appears at the right when groups don't fit; opens the collapsed groups. */
.scr-more { align-self: center; margin-left: auto; flex: 0 0 auto; }
.scr-more-menu { display: flex; flex-direction: column; gap: 8px; padding: 8px; max-height: 70vh;
  overflow: auto; min-width: 0; }
.scr-more-menu .scr-group { padding: 0; }
.scr-more-menu .scr-group-label { text-align: left; padding: 2px 0 0; }
/* Small button (the default): a compact icon (icon-only is square; icon+label left-aligns so stacked
   buttons grid up). Fluent small-control height. */
.scr-btn { font: inherit; min-width: var(--scr-ctl-h); height: var(--scr-ctl-h); padding: 0 6px;
  border: 1px solid transparent; border-radius: var(--scr-radius-sm); background: transparent;
  cursor: pointer; color: var(--scr-text); display: inline-flex; align-items: center;
  justify-content: center; gap: 6px; }
.scr-btn:hover { background: var(--scr-hover); border-color: var(--scr-border); }
.scr-btn:active { background: var(--scr-accent-bg); }
.scr-btn:focus-visible { outline: 2px solid var(--scr-accent-border); outline-offset: -1px; }
.scr-btn.active { background: var(--scr-accent-bg); border-color: var(--scr-accent-border); }
.scr-btn svg { flex: 0 0 auto; }
.scr-btn-label { font-size: 12px; line-height: 1; white-space: nowrap; }
.scr-btn-labeled { justify-content: flex-start; padding: 0 8px; }
/* Large button (Word's Paste / New Comment): a big icon over a label, spanning the full group body. */
.scr-large, .scr-paste { flex-direction: column; gap: 3px; height: auto; align-self: stretch;
  min-width: 56px; padding: 5px 8px; justify-content: center; }
.scr-large .scr-btn-label, .scr-paste .scr-btn-label { font-size: 11px; }
.scr-large svg, .scr-paste svg { margin-bottom: 1px; }

/* Split button (Word's Accept/Reject): primary action + a caret that opens a small menu. */
.scr-split { position: relative; display: inline-flex; }
.scr-split-main { border-radius: 5px 0 0 5px; padding: 0 8px; justify-content: flex-start; }
.scr-split-caret { min-width: 18px; padding: 0; border-radius: 0 5px 5px 0; margin-left: -1px; }
.scr-split:hover > .scr-btn { border-color: var(--scr-border); background: var(--scr-hover); }
.scr-menu { position: fixed; z-index: 2147483647; background: var(--scr-surface);
  border: 1px solid var(--scr-border-strong); border-radius: 6px; box-shadow: var(--scr-shadow-menu); padding: 4px;
  min-width: 184px; }
.scr-menu-item { display: flex; align-items: center; gap: 8px; width: 100%; text-align: left; border: 0;
  background: none; padding: 6px 10px; border-radius: 4px; cursor: pointer; color: var(--scr-text); font: inherit;
  font-size: 13px; }
.scr-menu-item:hover { background: var(--scr-hover); }
.scr-menu-item.active { background: var(--scr-accent-bg); }
.scr-num-sample { font-variant-numeric: tabular-nums; letter-spacing: 0.5px; }

/* Text-highlight palette: a grid of color swatches + a "No Color" row, dropped from the split button.
   Override the menu's 184px min-width so the flyout hugs the swatch grid instead of being oversized. */
.scr-hl-menu { padding: 6px; min-width: 0; width: max-content; }
.scr-hl-grid { display: grid; grid-template-columns: repeat(4, 22px); gap: 4px; }
.scr-hl-swatch { width: 22px; height: 22px; border: 1px solid var(--scr-border-strong); border-radius: 4px; cursor: pointer;
  padding: 0; }
.scr-hl-swatch:hover { outline: 2px solid var(--scr-accent-border); }
.scr-hl-swatch.active { outline: 2px solid var(--scr-accent); }
.scr-hl-none { margin-top: 6px; justify-content: center; }

/* Styles gallery: a flyout of live-preview cards, each name rendered in its own style. */
.scr-style-trigger { min-width: 92px; max-width: 164px; justify-content: space-between; }
.scr-style-trigger .scr-btn-label { overflow: hidden; text-overflow: ellipsis; }
.scr-style-gallery { position: fixed; z-index: 2147483647; background: var(--scr-surface);
  border: 1px solid var(--scr-border-strong); border-radius: 6px; box-shadow: var(--scr-shadow-menu); padding: 5px;
  display: grid; grid-template-columns: repeat(2, 168px); gap: 5px; max-height: 60vh; overflow-y: auto; }
.scr-style-card { display: flex; align-items: center; min-height: 38px; padding: 4px 10px; border: 1px solid var(--scr-border-subtle);
  border-radius: 5px; background: var(--scr-surface); cursor: pointer; text-align: left; overflow: hidden; }
.scr-style-card:hover { background: var(--scr-hover); border-color: #c4ccdb; }
.scr-style-card.active { background: var(--scr-accent-bg); border-color: var(--scr-accent-border); }
.scr-style-card-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; line-height: 1.2; }
.scr-style-modify { grid-column: 1 / -1; margin-top: 3px; padding: 6px 10px; border: 1px solid var(--scr-border);
  border-radius: 5px; background: var(--scr-surface); cursor: pointer; font: inherit; color: var(--scr-text);
  text-align: center; }
.scr-style-modify:hover { background: var(--scr-hover); border-color: #c4ccdb; }
.scr-b { font-weight: 700; } .scr-i { font-style: italic; } .scr-u { text-decoration: underline; }
.scr-s { text-decoration: line-through; }
.scr-select { font: inherit; height: var(--scr-ctl-h); border: 1px solid var(--scr-border);
  border-radius: var(--scr-radius-sm); background: var(--scr-surface); padding: 0 4px; }
.scr-family { width: 130px; } .scr-size { width: 58px; }
/* Styled dropdown (native-select replacement): a select-looking trigger + a scr-menu flyout. */
.scr-dd { font: inherit; height: var(--scr-ctl-h); min-width: 56px; border: 1px solid var(--scr-border);
  border-radius: var(--scr-radius-sm); background: var(--scr-surface); padding: 0 4px 0 8px;
  display: inline-flex; align-items: center; gap: 4px; justify-content: space-between; cursor: pointer;
  color: var(--scr-text); box-sizing: border-box; }
.scr-dd:hover { border-color: var(--scr-border-strong); }
.scr-dd:focus-within { border-color: var(--scr-accent-border); outline: 1px solid var(--scr-accent-border); }
.scr-dd-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.scr-dd-caret { display: inline-flex; align-items: center; color: var(--scr-text-subtle); flex: 0 0 auto; }
.scr-dd.scr-combo { padding-left: 0; }
.scr-dd-input { font: inherit; border: 0; outline: none; background: transparent; min-width: 0; width: 100%;
  padding: 0 0 0 8px; color: var(--scr-text); }
.scr-dd-caret { background: none; border: 0; padding: 0 2px; cursor: pointer; }
.scr-dd-menu { min-width: 0; max-height: 60vh; overflow-y: auto; }
.scr-dd-menu .scr-menu-item { padding: 5px 12px; }
.scr-color { width: 28px; height: var(--scr-ctl-h); padding: 1px; border: 1px solid var(--scr-border); border-radius: var(--scr-radius-sm); background: var(--scr-surface); cursor: pointer; }
.scr-labeled { display: flex; align-items: center; gap: 6px; }
/* Fixed-width label so the controls beside stacked labels (Size / Margins / Units, Display) line up
   in a column instead of starting at different x. */
.scr-labeled-text { color: var(--scr-text-muted); font-size: 12px; min-width: 52px; flex: 0 0 auto; }
.scr-panel.scr-readonly { opacity: 0.5; pointer-events: none; }

/* Workspace frame: ribbon, a fixed top band (horizontal ruler), a scrollable page stage with the
   vertical ruler beside the pages, then a status bar. */
.scr-workspace { display: flex; flex-direction: column; height: 100%; min-height: 0;
  font-family: var(--scr-font); }
.scr-top { flex: 0 0 auto; display: flex; justify-content: center; background: var(--scr-bg-stage);
  padding-top: 12px; overflow: hidden; }
.scr-top-row { display: flex; }
.scr-stage { flex: 1; min-height: 0; overflow: auto; background: var(--scr-bg-stage); padding: 0 0 28px; }
.scr-stage-inner { margin: 0 auto; width: max-content; }
.scr-corner { flex: 0 0 auto; background: var(--scr-bg); }
.scr-prow { display: flex; }
.scr-hruler, .scr-vruler { display: block; background: var(--scr-bg); touch-action: none; }
.scr-vruler { cursor: ns-resize; }
/* The page host holds the view's sheet; each page now carries its own drop shadow (per-page frames
   in the core view), so no single container shadow around the whole block. */
.scr-page-host { position: relative; }
.scr-ruler-tip { position: fixed; z-index: 1000; pointer-events: none; background: #2a2f3a; color: var(--scr-surface);
  font: 11px system-ui, sans-serif; padding: 2px 6px; border-radius: 4px; }

/* Read-only viewer frame: the same scrolling page stage as the workspace, but
   with no ribbon / rulers / status bar - just the rendered pages, padded top so
   the first page isn't flush against the edge. */
.scr-viewer { display: flex; flex-direction: column; height: 100%; min-height: 0;
  font-family: var(--scr-font); }
.scr-viewer .scr-stage { padding-top: 16px; }

/* Side-by-side comparison view: a nav/label bar above two independently-scrolling read-only panes.
   (Named scr-compareview so it doesn't collide with the Compare ribbon button's scr-compare class.) */
.scr-compareview { display: flex; flex-direction: column; flex: 1; min-height: 0;
  font-family: var(--scr-font); background: var(--scr-bg-stage); }
.scr-compare-bar { position: relative; box-sizing: border-box; display: flex; align-items: center;
  min-height: 36px; padding: 4px 12px; background: var(--scr-bg);
  border-bottom: 1px solid var(--scr-border-chrome); font-size: 12px; flex: 0 0 auto; }
/* Two labels, each centered over its pane (flex:1), so they align with the split below. A long
   source name ellipsizes rather than overflowing the column (and clears the floated controls). */
.scr-compare-label { flex: 1; min-width: 0; text-align: center; font-weight: 600;
  color: var(--scr-text-muted); letter-spacing: 0.04em; font-size: 11px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 0 8px; }
.scr-compare-label:last-of-type { padding-right: 96px; } /* keep clear of the nav/close controls */
/* The change-nav + close float at the right edge without disturbing the label columns. */
.scr-compare-controls { position: absolute; right: 10px; top: 50%; transform: translateY(-50%);
  display: inline-flex; align-items: center; gap: 6px; }
.scr-compare-close { margin-left: 4px; }
.scr-compare-count { min-width: 68px; text-align: center; color: var(--scr-ink); font-variant-numeric: tabular-nums; }
.scr-compare-navbtn { display: inline-flex; align-items: center; justify-content: center;
  width: 24px; height: 24px; border: 1px solid var(--scr-border-chrome); border-radius: 5px;
  background: var(--scr-bg); color: var(--scr-ink); cursor: pointer; }
.scr-compare-navbtn:hover { background: var(--scr-hover); }
.scr-compare-split { display: flex; flex: 1; min-height: 0; }
.scr-compare-pane { flex: 1; min-width: 0; min-height: 0; display: flex;
  border-left: 1px solid var(--scr-border-subtle); }
.scr-compare-pane:first-child { border-left: 0; }
/* min-width: 0 so a pane narrower than a page sheet shrinks and its stage scrolls horizontally,
   instead of the viewer's intrinsic (full-page) width overflowing the split and overlapping the
   other pane (flexbox's automatic minimum size would otherwise refuse to shrink it). */
.scr-compare-pane .scr-viewer { flex: 1; min-width: 0; }

/* Status bar */
.scr-status { display: flex; align-items: center; gap: 16px; padding: 4px 12px; font-size: 12px;
  background: var(--scr-bg); border-top: 1px solid var(--scr-border-chrome); color: var(--scr-text-muted); }
.scr-status .scr-spacer { margin-left: auto; }
.scr-status-sep { color: var(--scr-text-disabled); }
.scr-zoom { display: flex; align-items: center; gap: 6px; }
.scr-zoom button { font: inherit; width: 22px; height: 22px; border: 1px solid var(--scr-border); border-radius: 4px;
  background: var(--scr-surface); cursor: pointer; line-height: 1; display: flex; align-items: center; justify-content: center;
  color: var(--scr-text-muted); }
.scr-zoom button svg { display: block; }
.scr-zoom-slider { width: 120px; cursor: pointer; }
.scr-zoom .scr-zoom-val { min-width: 40px; text-align: right; }

/* Workspace body row: the page area (rulers + stage) beside an optional docked reviewing pane. */
.scr-body { display: flex; flex: 1; min-height: 0; }
.scr-main { display: flex; flex-direction: column; flex: 1; min-width: 0; min-height: 0; }

/* Reviewing pane: a docked side list of every tracked change + comment. */
.scr-reviewing-pane { position: relative; flex: 0 0 304px; display: flex; flex-direction: column;
  min-height: 0; background: var(--scr-surface); border-left: 1px solid var(--scr-border-chrome);
  font-family: var(--scr-font); }
/* Left-edge resize handle: a slim hit-target straddling the border; highlights on hover / drag. */
.scr-rp-resize { position: absolute; left: -3px; top: 0; bottom: 0; width: 7px; cursor: col-resize;
  z-index: 3; touch-action: none; }
.scr-rp-resize::after { content: ''; position: absolute; left: 3px; top: 0; bottom: 0; width: 1px; }
.scr-rp-resize:hover::after, .scr-rp-resize.dragging::after { background: var(--scr-accent); }
.scr-rp-head { display: flex; align-items: center; justify-content: space-between; padding: 9px 12px;
  border-bottom: 1px solid var(--scr-border-subtle); font-weight: 600; color: var(--scr-text-muted); font-size: 13px; }
.scr-rp-title { display: flex; align-items: center; gap: 8px; }
.scr-rp-count { background: var(--scr-hover-strong); color: var(--scr-text-muted); border-radius: 10px; padding: 0 8px; font-size: 11px; }
.scr-rp-close { border: 0; background: none; cursor: pointer; color: var(--scr-text-subtle); display: flex; padding: 2px; }
.scr-rp-close:hover { color: var(--scr-text-muted); }
.scr-rp-legend { border-bottom: 1px solid var(--scr-border-subtle); padding: 6px 12px; display: flex; flex-wrap: wrap;
  gap: 4px 14px; }
.scr-rp-rev { display: inline-flex; align-items: center; gap: 6px; cursor: pointer; font-size: 12px;
  color: var(--scr-text-muted); }
.scr-rp-rev input { margin: 0; cursor: pointer; }
.scr-rp-rev-off .scr-rp-rev-name { color: var(--scr-text-faint); text-decoration: line-through; }
.scr-rp-swatch { width: 11px; height: 11px; border-radius: 3px; flex: 0 0 auto;
  box-shadow: inset 0 0 0 1px rgba(0,0,0,.12); }
.scr-rp-list { flex: 1; overflow: auto; padding: 8px; }
.scr-rp-empty { color: var(--scr-text-subtle); text-align: center; padding: 28px 12px; font-size: 13px; }
/* Comparison summary bar: one or two rows (comparison-scope line, semantic-overlay line). */
.scr-rp-summary { display: flex; flex-direction: column; gap: 5px;
  padding: 7px 12px; border-bottom: 1px solid var(--scr-border-subtle); font-size: 12px; }
.scr-rp-sum-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.scr-rp-sum-counts { font-weight: 600; color: var(--scr-ink); }
.scr-rp-sum-filter { display: inline-flex; align-items: center; gap: 5px; color: var(--scr-text-muted); cursor: pointer; }
.scr-rp-mat { font-size: 10px; font-weight: 700; padding: 1px 7px; border-radius: 9px;
  color: var(--scr-text-muted); background: var(--scr-hover); }
.scr-rp-mat-sub { color: #92400e; background: #fef0c7; }
.scr-rp-ann-summary { font-size: 12px; color: var(--scr-text); font-style: italic; margin: 4px 0 2px; }
.scr-rp-risk { display: flex; align-items: center; gap: 5px; font-size: 11.5px; color: #b42318; margin-top: 2px; }
.scr-rp-risk svg { flex: none; }
.scr-rp-item { border: 1px solid var(--scr-border-subtle); border-radius: 8px; padding: 8px 10px; margin-bottom: 7px;
  cursor: pointer; }
.scr-rp-item:hover { background: #f7f9fc; border-color: #cfd6e4; }
.scr-rp-resolved { opacity: 0.62; }
.scr-rp-head-row { display: flex; align-items: center; gap: 7px; margin-bottom: 4px; }
.scr-rp-kind { font-size: 10px; font-weight: 700; letter-spacing: 0.03em; padding: 1px 7px; border-radius: 9px; }
.scr-rp-author { font-size: 12px; font-weight: 600; color: var(--scr-text); }
.scr-rp-date { margin-left: auto; font-size: 11px; color: var(--scr-text-subtle); }
.scr-rp-text { font-size: 13px; color: var(--scr-ink); white-space: pre-wrap; word-break: break-word;
  display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 3; overflow: hidden; }
.scr-rp-text-open { display: block; -webkit-line-clamp: unset; }
.scr-rp-more { font: inherit; font-size: 11px; border: 0; background: none; padding: 2px 0 0;
  color: var(--scr-accent, #2563eb); cursor: pointer; }
.scr-rp-more:hover { text-decoration: underline; }
.scr-rp-actions { display: flex; gap: 6px; margin-top: 8px; }
.scr-rp-btn { font: inherit; font-size: 12px; border: 1px solid var(--scr-border); background: var(--scr-surface); color: var(--scr-text);
  border-radius: 6px; padding: 3px 9px; cursor: pointer; display: inline-flex; align-items: center; gap: 4px; }
.scr-rp-btn:hover { background: var(--scr-hover); }
.scr-rp-btn svg { width: 14px; height: 14px; }
.scr-rp-danger:hover { background: var(--scr-danger-bg); color: var(--scr-danger); border-color: var(--scr-danger-border); }

/* Page Setup dialog (Layout tab launcher): a centered modal over a dimmed backdrop. */
.scr-modal-backdrop { position: fixed; inset: 0; z-index: 2147483647; background: rgba(20,28,48,0.32);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--scr-font); }
.scr-modal { box-sizing: border-box; background: var(--scr-surface); border-radius: 10px;
  box-shadow: var(--scr-shadow-modal); padding: 18px 20px; width: 340px;
  max-width: calc(100vw - 32px); color: var(--scr-text); }
.scr-modal *, .scr-modal *::before, .scr-modal *::after { box-sizing: border-box; }
.scr-modal-title { font-size: 15px; font-weight: 600; margin-bottom: 12px; }
.scr-modal-field { display: flex; flex-direction: column; gap: 3px; margin-bottom: 10px; min-width: 0; }
.scr-modal-label { font-size: 11px; color: var(--scr-text-muted); }
.scr-modal-margins { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 0 12px; }
.scr-modal-num { font: inherit; width: 100%; height: 28px; border: 1px solid var(--scr-border); border-radius: 5px;
  padding: 0 8px; }
.scr-modal-text { font: inherit; width: 100%; height: 28px; border: 1px solid var(--scr-border); border-radius: 5px;
  padding: 0 8px; box-sizing: border-box; }
.scr-modal-color { width: 100%; height: 28px; border: 1px solid var(--scr-border); border-radius: 5px;
  padding: 2px; background: var(--scr-surface); cursor: pointer; }
.scr-modal-seg-group { display: flex; width: 100%; border: 1px solid var(--scr-border); border-radius: 6px;
  overflow: hidden; }
.scr-modal-seg { font: inherit; flex: 1; height: 30px; border: 0; background: var(--scr-surface); cursor: pointer;
  color: var(--scr-text-muted); }
.scr-modal-seg + .scr-modal-seg { border-left: 1px solid var(--scr-border); }
.scr-modal-seg:hover { background: var(--scr-hover); }
.scr-modal-seg.active { background: var(--scr-accent-bg); color: var(--scr-accent-text); font-weight: 600; }
.scr-modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 14px; }
.scr-modal-btn { font: inherit; border: 1px solid var(--scr-border-strong); background: var(--scr-surface); color: var(--scr-ink);
  padding: 6px 14px; border-radius: 6px; cursor: pointer; }
.scr-modal-primary { border: 0; background: var(--scr-accent); color: var(--scr-surface); }
.scr-modal-btn:disabled { opacity: 0.6; cursor: default; }

/* Compare Documents dialog: two columns (Original → Revised), each a document card / drop target -
   mirroring the side-by-side result so the user pictures the diff before running it. */
.scr-compare-modal { width: 560px; max-width: calc(100vw - 32px); }
.scr-cmp-cols { display: flex; align-items: stretch; gap: 10px; margin: 2px 0 6px; }
.scr-cmp-col { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 6px; }
.scr-cmp-col-head { font-size: 11px; font-weight: 700; letter-spacing: 0.04em; text-transform: uppercase;
  color: var(--scr-text-muted); }
.scr-cmp-arrow { align-self: center; display: flex; align-items: center; color: var(--scr-text-muted);
  padding-top: 20px; }
.scr-cmp-card { position: relative; box-sizing: border-box; min-height: 118px; display: flex;
  align-items: center; justify-content: center; padding: 12px; border-radius: 8px;
  border: 1.5px dashed var(--scr-border-strong); background: var(--scr-surface);
  transition: border-color 120ms, background 120ms; }
.scr-cmp-card.scr-cmp-card-filled { border-style: solid; border-color: var(--scr-accent); }
.scr-cmp-card.scr-drop-over { border-color: var(--scr-accent); background: var(--scr-accent-bg); }
.scr-drop-input { position: absolute; width: 0; height: 0; opacity: 0; }
.scr-cmp-drop { display: flex; flex-direction: column; align-items: center; gap: 6px; text-align: center; }
.scr-cmp-drop-label { font-size: 12px; color: var(--scr-text-muted); }
.scr-drop-browse { font: inherit; font-size: 12px; border: 0; background: none; padding: 0;
  color: var(--scr-accent); text-decoration: underline; cursor: pointer; }
.scr-cmp-filled { display: flex; align-items: center; gap: 10px; width: 100%; color: var(--scr-ink); }
.scr-cmp-filled svg { flex: 0 0 auto; color: var(--scr-accent); }
.scr-cmp-filled-txt { min-width: 0; }
.scr-cmp-name { font-weight: 600; font-size: 13px; word-break: break-word; }
.scr-cmp-sub { font-size: 11px; color: var(--scr-text-muted); }
.scr-cmp-clear { flex: 0 0 auto; margin-left: auto; display: inline-flex; align-items: center;
  justify-content: center; width: 24px; height: 24px; border: 1px solid var(--scr-border-chrome);
  border-radius: 5px; background: var(--scr-bg); color: var(--scr-text-muted); cursor: pointer; }
.scr-cmp-clear:hover { background: var(--scr-hover); color: var(--scr-ink); }
.scr-cmp-alts { display: flex; flex-wrap: wrap; gap: 6px; justify-content: center; margin-top: 2px; }
.scr-cmp-alt { font: inherit; font-size: 12px; padding: 2px 9px; cursor: pointer;
  border: 1px solid var(--scr-border-chrome); border-radius: 12px;
  background: var(--scr-bg); color: var(--scr-text); }
.scr-cmp-alt:hover { background: var(--scr-hover); border-color: var(--scr-accent); }
.scr-cmp-opts { display: flex; flex-wrap: wrap; gap: 6px 16px; margin: 2px 0 4px;
  padding-top: 8px; border-top: 1px solid var(--scr-border-subtle); }
.scr-cmp-opt { display: inline-flex; align-items: center; gap: 5px; font-size: 12px;
  color: var(--scr-text); cursor: pointer; }
.scr-compare-err { font-size: 12px; color: #b42318; min-height: 16px; margin-top: 2px; }
`;
