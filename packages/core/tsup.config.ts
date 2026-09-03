import { defineConfig } from 'tsup';

// Build to dist/ for cross-app vendoring. Within the monorepo, packages
// resolve each other's TS source via the `main` -> ./src/index.ts entry, so dev needs no build.
// The WASM package is external: the consuming app's bundler resolves the .wasm asset.
export default defineConfig({
  entry: ['src/index.ts'],
  format: ['esm'],
  dts: true,
  clean: true,
  treeshake: true,
  external: ['@truespar/scriptor-wasm'],
});
