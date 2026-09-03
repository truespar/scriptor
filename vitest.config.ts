import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vitest/config';

// Two projects, because the package has two genuinely different kinds of code.
//
// `unit` is everything that is a pure function of its inputs - region encoding, comparison
// annotation mapping, formatting helpers. It runs in Node in milliseconds and needs nothing.
//
// `browser` is the view itself. Scriptor paints to a `<canvas>` through WebAssembly and decodes
// pictures with `createImageBitmap`, none of which jsdom implements: a jsdom run would either fail
// or pass against so many mocks that it proves nothing. Browser mode runs the real thing in real
// Chromium, so a green test means the editor actually mounted and rendered.
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: 'unit',
          environment: 'node',
          include: ['packages/*/src/**/*.test.ts'],
          exclude: ['**/*.browser.test.ts'],
        },
      },
      {
        test: {
          name: 'browser',
          include: ['packages/*/src/**/*.browser.test.ts'],
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            instances: [{ browser: 'chromium' }],
          },
        },
      },
    ],
  },
});
