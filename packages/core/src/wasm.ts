// One-time WASM initialization, shared by every view on the page.
//
// The engine module is a singleton: the first view to need it starts the load and everyone else
// awaits the same promise. Once it is live the bundled substitute faces are registered as DOM
// @font-face rules, so host chrome (font menu, style gallery) previews in the same clone the
// canvas paints with.
import init, { fontFaces } from '@truespar/scriptor-wasm';

let wasmReady: Promise<unknown> | null = null;
export function ensureWasm(): Promise<unknown> {
  if (!wasmReady)
    wasmReady = init().then((m) => {
      registerBundledFonts();
      return m;
    });
  return wasmReady;
}

/** Register every bundled substitute face (Carlito, Caladea, ..., the TeX Gyre legal serifs) as a DOM
 *  `@font-face` under its MS family name, so DOM text - the ribbon's font dropdown, the style gallery
 *  cards - previews in the exact clone the wasm shaper renders on the canvas (the OS has none of these
 *  MS fonts). Idempotent and SSR-safe (a no-op without `document` / `FontFace`); each face loads in
 *  the background, so it never blocks the view's first paint. */
let fontsRegistered = false;
function registerBundledFonts(): void {
  if (fontsRegistered) return;
  if (typeof document === 'undefined' || typeof FontFace === 'undefined' || !document.fonts) return;
  fontsRegistered = true;
  const faces = fontFaces() as Array<{
    family: string;
    bold: boolean;
    italic: boolean;
    bytes: Uint8Array;
  }>;
  for (const f of faces) {
    try {
      // `bytes` is already an independent JS-heap copy (the wasm export copies out), so FontFace can
      // hold it safely - no aliasing into wasm linear memory.
      const face = new FontFace(f.family, f.bytes as unknown as BufferSource, {
        weight: f.bold ? 'bold' : 'normal',
        style: f.italic ? 'italic' : 'normal',
      });
      face
        .load()
        .then((loaded) => document.fonts.add(loaded))
        .catch(() => {
          /* a face that fails to parse just falls back to the system font in previews */
        });
    } catch {
      /* FontFace ctor rejected the descriptor - skip this face, previews degrade gracefully */
    }
  }
}

// ── document comparison (blacklining) ─────────────────────────────────────────
