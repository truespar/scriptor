import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  type CSSProperties,
} from 'react';
import {
  ScriptorView,
  type ScriptorMode,
  type Selection,
} from '@truespar/scriptor-core';

export interface ScriptorDocProps {
  /** Document to render (raw `.docx` OPC zip bytes). */
  docx?: Uint8Array;
  /** `'read'` (default) or `'edit'`. */
  mode?: ScriptorMode;
  gutter?: string;
  selectable?: boolean;
  onChange?: () => void;
  onSelectionChange?: (selection: Selection | null) => void;
  onReady?: () => void;
  className?: string;
  style?: CSSProperties;
}

/** Imperative handle exposed via `ref`. */
export interface ScriptorDocHandle {
  loadDocx(bytes: Uint8Array): void;
  toDocumentXml(): string;
}

/** React wrapper around the headless [`ScriptorView`]. */
export const ScriptorDoc = forwardRef<ScriptorDocHandle, ScriptorDocProps>(function ScriptorDoc(
  props,
  ref,
) {
  const elRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<ScriptorView | null>(null);
  // Latest callbacks, read indirectly so the view is created once but always calls current handlers.
  const cb = useRef(props);
  cb.current = props;

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    let view: ScriptorView | null = null;
    let cancelled = false;
    void ScriptorView.create(el, {
      mode: cb.current.mode ?? 'read',
      gutter: cb.current.gutter,
      selectable: cb.current.selectable,
      onChange: () => cb.current.onChange?.(),
      onSelectionChange: (s) => cb.current.onSelectionChange?.(s),
      onReady: () => cb.current.onReady?.(),
    }).then((v) => {
      if (cancelled) {
        v.destroy();
        return;
      }
      view = v;
      viewRef.current = v;
      if (cb.current.docx) v.loadDocx(cb.current.docx);
    });
    return () => {
      cancelled = true;
      view?.destroy();
      viewRef.current = null;
    };
  }, []);

  useEffect(() => {
    viewRef.current?.setMode(props.mode ?? 'read');
  }, [props.mode]);

  useEffect(() => {
    if (props.docx) viewRef.current?.loadDocx(props.docx);
  }, [props.docx]);

  useImperativeHandle(
    ref,
    () => ({
      loadDocx: (bytes: Uint8Array) => viewRef.current?.loadDocx(bytes),
      toDocumentXml: () => viewRef.current?.toDocumentXml() ?? '',
    }),
    [],
  );

  return <div ref={elRef} className={props.className} style={props.style} />;
});
