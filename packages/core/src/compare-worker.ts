/**
 * Web Worker that runs the (synchronous, potentially heavy) wasm comparison off the main thread, so a
 * large compare never freezes the browser tab. It loads its own wasm instance on first use. The main
 * thread talks to it through [`compareDocuments`] (see `view.ts`), which falls back to a synchronous
 * main-thread compare when a Worker can't be created (SSR, or a bundler that doesn't carry the worker).
 */
import init, { compareDocx } from '@truespar/scriptor-wasm';

interface CompareRequest {
  id: number;
  original: Uint8Array;
  revised: Uint8Array;
  author: string;
  date: string;
  detectFormatting: boolean;
  detectMoves: boolean;
  ignoreWhitespace: boolean;
  ignoreCase: boolean;
}

let ready: Promise<unknown> | null = null;

self.addEventListener('message', (event: MessageEvent<CompareRequest>) => {
  const { id, original, revised, author, date } = event.data;
  const { detectFormatting, detectMoves, ignoreWhitespace, ignoreCase } = event.data;
  if (!ready) ready = init();
  ready
    .then(() => {
      const raw = compareDocx(
        original,
        revised,
        author,
        date,
        detectFormatting,
        detectMoves,
        ignoreWhitespace,
        ignoreCase,
      ) as {
        redline: Uint8Array;
        manifest: string;
      };
      // Transfer the redline buffer back (no copy); the manifest is a small string.
      (self as unknown as Worker).postMessage(
        { id, redline: raw.redline, manifest: raw.manifest },
        [raw.redline.buffer],
      );
    })
    .catch((err: unknown) => {
      (self as unknown as Worker).postMessage({
        id,
        error: String((err as Error)?.message ?? err),
      });
    });
});
