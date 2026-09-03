import { defineConfig } from 'vite';
import topLevelAwait from 'vite-plugin-top-level-await';
import wasm from 'vite-plugin-wasm';

// Demo app consuming @truespar/scriptor-core. The core pulls in @truespar/scriptor-wasm (the
// wasm-pack output); vite-plugin-wasm + top-level-await let Vite serve the .wasm asset, and both
// scriptor packages are excluded from dep pre-bundling so esbuild never tries to scan the wasm.
// fs.allow lets Vite read the sibling packages/ + their dist from outside the app dir.
//
// `Cache-Control: no-store`: the generated wasm glue fetches `scriptor_wasm_bg.wasm` from a stable,
// unhashed URL, so the browser would HTTP-cache it and intermittently serve a stale binary after a
// rebuild. no-store on the dev server forces every reload to refetch the current wasm (this is the
// demo app; production vendors the dist, so it never hits this path).
export default defineConfig({
  plugins: [wasm(), topLevelAwait()],
  server: {
    port: 5174,
    open: true,
    fs: { allow: ['../..'] },
    headers: { 'Cache-Control': 'no-store' },
  },
  optimizeDeps: { exclude: ['@truespar/scriptor-core', '@truespar/scriptor-wasm'] },
  build: { target: 'esnext' },
});
