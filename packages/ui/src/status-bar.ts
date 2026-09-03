import type { ScriptorView } from '@truespar/scriptor-core';
import { h } from './dom';
import { icon, type IconName } from './icons';

/** The bottom status bar: "Page X of N", word count, and a right-aligned zoom slider - read from
 *  the view. Mirrors Word's footer layout. */
export class StatusBar {
  readonly element: HTMLElement;
  private readonly view: ScriptorView;
  private readonly pagesEl: HTMLElement;
  private readonly wordsEl: HTMLElement;
  private readonly slider: HTMLInputElement;
  private readonly zoomVal: HTMLElement;
  private readonly unsub: () => void;

  constructor(view: ScriptorView) {
    this.view = view;
    this.element = h('div', { class: 'scr-status' });

    this.pagesEl = h('span');
    const div1 = h('span', { class: 'scr-status-sep' });
    div1.textContent = '|';
    this.wordsEl = h('span');
    const spacer = h('span', { class: 'scr-spacer' });

    // Zoom control: − / slider / + / value.
    const zoom = h('div', { class: 'scr-zoom' });
    const minus = zbtn('minus', 'Zoom out', () => view.setZoom(round1(view.zoomLevel - 0.1)));
    this.slider = h('input', {
      class: 'scr-zoom-slider',
      type: 'range',
      min: '25',
      max: '400',
      step: '5',
      title: 'Zoom',
    }) as HTMLInputElement;
    this.slider.addEventListener('input', () => view.setZoom(Number(this.slider.value) / 100));
    const plus = zbtn('plus', 'Zoom in', () => view.setZoom(round1(view.zoomLevel + 0.1)));
    this.zoomVal = h('span', { class: 'scr-zoom-val' });
    zoom.append(minus, this.slider, plus, this.zoomVal);

    this.element.append(this.pagesEl, div1, this.wordsEl, spacer, zoom);
    this.unsub = view.addListener(() => this.refresh());
    this.refresh();
  }

  refresh(): void {
    const pages = Math.max(1, this.view.pageCount());
    this.pagesEl.textContent = `Page ${this.view.currentPage()} of ${pages}`;
    const words = this.view.wordCount();
    this.wordsEl.textContent = `${words} ${words === 1 ? 'word' : 'words'}`;
    const pct = Math.round(this.view.zoomLevel * 100);
    this.slider.value = String(pct);
    this.zoomVal.textContent = `${pct}%`;
  }

  destroy(): void {
    this.unsub();
    this.element.remove();
  }
}

function zbtn(name: IconName, title: string, onClick: () => void): HTMLButtonElement {
  const b = h('button', { type: 'button', title }) as HTMLButtonElement;
  b.append(icon(name, 14));
  b.addEventListener('click', onClick);
  return b;
}

function round1(n: number): number {
  return Math.round(n * 10) / 10;
}
