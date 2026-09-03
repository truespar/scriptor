import type { Action } from 'svelte/action';
import {
  ScriptorView,
  type ScriptorMode,
  type Selection,
} from '@truespar/scriptor-core';

/** Parameters for the [`scriptor`] action. */
export interface ScriptorActionParams {
  docx?: Uint8Array;
  mode?: ScriptorMode;
  gutter?: string;
  selectable?: boolean;
  onChange?: () => void;
  onSelectionChange?: (selection: Selection | null) => void;
  onReady?: () => void;
}

/**
 * Svelte action mounting a [`ScriptorView`] into the node:
 *
 *   <div use:scriptor={{ mode: 'edit', docx }}></div>
 */
export const scriptor: Action<HTMLElement, ScriptorActionParams | undefined> = (
  node,
  initial = {},
) => {
  let params = initial;
  let view: ScriptorView | null = null;
  let destroyed = false;

  void ScriptorView.create(node, {
    mode: params.mode,
    gutter: params.gutter,
    selectable: params.selectable,
    onChange: () => params.onChange?.(),
    onSelectionChange: (s) => params.onSelectionChange?.(s),
    onReady: () => params.onReady?.(),
  }).then((v) => {
    if (destroyed) {
      v.destroy();
      return;
    }
    view = v;
    if (params.docx) v.loadDocx(params.docx);
  });

  return {
    update(next: ScriptorActionParams | undefined = {}): void {
      const prev = params;
      params = next;
      if (next.mode && next.mode !== prev.mode) view?.setMode(next.mode);
      if (next.docx && next.docx !== prev.docx) view?.loadDocx(next.docx);
    },
    destroy(): void {
      destroyed = true;
      view?.destroy();
    },
  };
};
