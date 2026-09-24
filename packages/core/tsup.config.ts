import { readFile, writeFile } from 'node:fs/promises';
import { defineConfig } from 'tsup';

// Build to dist/ for cross-app vendoring. Within the monorepo, packages
// resolve each other's TS source via the `main` -> ./src/index.ts entry, so dev needs no build.
// The WASM package is external: the consuming app's bundler resolves the .wasm asset.
export default defineConfig({
  // The worker is a second entry, not an accident of bundling: `compare.ts` reaches it
  // through `new URL(..., import.meta.url)`, which is a runtime address rather than an
  // import, so nothing would otherwise emit it.
  entry: ['src/index.ts', 'src/compare-worker.ts'],
  format: ['esm'],
  dts: true,
  clean: true,
  treeshake: true,
  external: ['@truespar/scriptor-wasm'],
  // That URL names `./compare-worker.ts`, which is right in the monorepo and right for
  // a bundler pointed at `src` — but esbuild copies `new URL(..., import.meta.url)`
  // through untouched, so in `dist` it named a neighbour that was not there. Vendoring
  // the built package got a 404 the first time anyone compared two documents, which is
  // the one thing dist/ exists for. Retarget the reference at the worker we just built.
  async onSuccess() {
    const bundle = 'dist/index.js';
    const before = await readFile(bundle, 'utf8');
    const after = before.replaceAll('./compare-worker.ts', './compare-worker.js');
    // Loudly, rather than silently shipping the bug again the day the worker is
    // renamed or the call is rewritten.
    if (after === before) {
      throw new Error(`no ./compare-worker.ts reference in ${bundle} — has the worker moved?`);
    }
    await writeFile(bundle, after);
  },
});
