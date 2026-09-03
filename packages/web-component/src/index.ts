import { ScriptorView, type ScriptorMode, type Selection } from '@truespar/scriptor-core';

/**
 * `<scriptor-doc>` - the framework-agnostic embed surface. Renders a Scriptor document onto its own
 * canvas (read-only by default; `mode="edit"` to edit). Load content with the `loadDocx(bytes)`
 * method (or set the `src` attribute to a URL). Emits `change`, `selectionchange`, and `ready`.
 *
 *   <scriptor-doc mode="edit"></scriptor-doc>
 *   document.querySelector('scriptor-doc').loadDocx(bytes)
 */
export class ScriptorDocElement extends HTMLElement {
  static readonly observedAttributes = ['mode'];

  private view: ScriptorView | null = null;
  private mountEl: HTMLDivElement | null = null;
  private pendingDocx: Uint8Array | null = null;

  connectedCallback(): void {
    const mount = document.createElement('div');
    this.appendChild(mount);
    this.mountEl = mount;
    void ScriptorView.create(mount, {
      mode: (this.getAttribute('mode') as ScriptorMode | null) ?? 'read',
      gutter: this.getAttribute('gutter') ?? undefined,
      selectable: this.getAttribute('selectable') !== 'false',
      onChange: () => this.dispatchEvent(new CustomEvent('change')),
      onSelectionChange: (s: Selection | null) =>
        this.dispatchEvent(new CustomEvent('selectionchange', { detail: s })),
      onReady: () => this.dispatchEvent(new CustomEvent('ready')),
    }).then((view) => {
      this.view = view;
      const src = this.getAttribute('src');
      if (this.pendingDocx) {
        view.loadDocx(this.pendingDocx);
        this.pendingDocx = null;
      } else if (src) {
        void fetch(src)
          .then((r) => r.arrayBuffer())
          .then((b) => view.loadDocx(new Uint8Array(b)));
      }
    });
  }

  disconnectedCallback(): void {
    this.view?.destroy();
    this.view = null;
    this.mountEl?.remove();
    this.mountEl = null;
  }

  attributeChangedCallback(name: string, _old: string | null, value: string | null): void {
    if (name === 'mode') this.view?.setMode((value as ScriptorMode | null) ?? 'read');
  }

  /** Load a `.docx` (raw OPC zip bytes). Queues until the view is ready if called early. */
  loadDocx(bytes: Uint8Array): void {
    if (this.view) this.view.loadDocx(bytes);
    else this.pendingDocx = bytes;
  }

  /** The current document body as OOXML `word/document.xml`. */
  toDocumentXml(): string {
    return this.view?.toDocumentXml() ?? '';
  }
}

/** Register the custom element (idempotent). Called automatically on import. */
export function defineScriptorDoc(tag = 'scriptor-doc'): void {
  if (typeof customElements !== 'undefined' && !customElements.get(tag)) {
    customElements.define(tag, ScriptorDocElement);
  }
}

defineScriptorDoc();
