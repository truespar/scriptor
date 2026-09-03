# Contributing to Scriptor

Contributions are welcome. There is no CLA and no contributor agreement to sign.

Scriptor is dual-licensed MIT OR Apache-2.0. Unless you say otherwise, anything you submit for
inclusion is contributed under those same two licenses, and you keep the copyright in what you
write. See [LICENSE](LICENSE).

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

## Reporting a document that renders wrong

This is the most useful kind of report, and it needs no code.

Scriptor is tested against 1,347 synthetic files from the LibreOffice test corpus. Those say very
little about real contracts, filings, academic papers and report templates, which is where the
remaining layout bugs are. If a document does not look the way Word shows it, open an issue with:

- What the document uses: numbered headings, a table spanning pages, a rotated stamp in the footer,
  and so on.
- What Word shows.
- What Scriptor shows.
- The file, if you can share it.

**You do not need to attach the file.** Most Word documents are confidential. Two screenshots, or a
plain description, is enough to start. If you want to send something reproducible, delete the body
text but keep the styles, tables, numbering, section breaks and headers. Layout bugs almost always
survive that, because they are rarely caused by the words.

## Setting up

Rust (stable), Node 22+, pnpm. For the browser editor you also need `wasm-pack` and the
`wasm32-unknown-unknown` target. For the schema validator you need the .NET 9 SDK.

```sh
cargo test --workspace          # ~370 tests, about a second
cargo clippy --workspace        # warning-free; keep it that way
pnpm install && pnpm build      # builds the wasm package, then the TS packages
pnpm typecheck
pnpm lint
```

On Windows, if linking fails with `LNK1104: cannot open file 'msvcrt.lib'`, dot-source
`scripts\dev-shell.ps1` once per shell and run cargo again.

The TypeScript tests run as two Vitest projects. `unit` covers the pure functions and runs in Node
in milliseconds. `browser` drives the real editor in real Chromium through Playwright, because
Scriptor paints to a `<canvas>` through WebAssembly and decodes pictures with `createImageBitmap`;
jsdom implements none of that, so a jsdom suite would either fail or pass against enough mocks to
prove nothing.

The browser project needs a one-time `pnpm exec playwright install chromium`. To run one project on
its own, use `pnpm exec vitest run --project unit`.

Coverage is thin - a smoke suite over mounting, the document lifecycle, and the view's extracted
controllers. Widening it is among the more useful things to contribute, since `packages/core` is
otherwise checked only by the compiler.

## Where to start

`crates/scriptor-layout` is where help goes furthest. The Status section of the README lists
specific open items, including body-paragraph page splitting, `w:evenAndOddHeaders`, and nested
tables. Picking a document that renders wrong and tracing it back also works well.

If you plan a large change, open an issue first so nobody duplicates your work. Small fixes can go
straight to a PR.

## Sending a pull request

1. Fork, branch, and make your change.
2. If you touched the OOXML export path, meaning anything that changes the bytes written into a
   `.docx`, run the corpus check described below. Include the result in the PR.
3. Open the PR against `main` and describe what changed and why. Link the issue if there is one.
4. Run the checks yourself before opening the PR. There is no CI on this repository, so nothing
   runs them for you and a reviewer's first look should not be spent on a failing build:

   ```sh
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo deny check          # licence, advisory and source policy - see deny.toml
   pnpm build && pnpm typecheck && pnpm lint && pnpm test
   ```
5. A maintainer reviews it. Push fixups to the same branch; there is no need to rebase or squash.

Reviews can take a while. If a PR has gone quiet for a month, ping it.

## The corpus check

A fidelity fix that improves one document often breaks forty others, so export-path changes are
checked against the whole corpus. The corpus is not vendored, because the LibreOffice `sw/qa` files
are MPL and bug-tracker attachments: usable as test inputs, not redistributable.

```sh
git clone --depth 1 https://git.libreoffice.org/core
pwsh -File scripts/corpus-gate.ps1 -Corpus <path>/sw/qa/extras/ooxmlexport/data
```

It should print `PASS`. If it reports improvements instead, rerun with `-Update` and commit the
refreshed baseline with your change.

This runs on Linux and macOS as well as Windows: `pwsh` is cross-platform, and the script needs the
.NET 9 SDK but not Microsoft Word. Only `scripts/word-*.ps1` require Windows and Word, and nothing
expects you to run those. See [`docs/testing.md`](docs/testing.md) for the full harness.

If you cannot run the check, send the PR anyway and say so. A maintainer will run it.

## Code style

Match the surrounding code. `cargo fmt` and `pnpm lint` handle the mechanical part. Clippy is
warning-free; if a lint fires, fix it or add a narrow `#[allow(...)]` with a comment saying why.

Two things matter more than style:

- Explain why, not what, and especially when the answer is "because Word does this". A lot of the
  engine looks wrong until you know which Word behaviour it reproduces. Name it, so the next person
  does not delete it as a bug.
- Nothing reaches `.docx` output unless it is modelled or passed through verbatim. Silent data loss
  is the bug class we treat as most serious. See [`docs/passthrough.md`](docs/passthrough.md).

## Use of AI

Using an AI assistant while you work is fine. Sending output you have not read is not.

You are responsible for every line in your PR, and you should be able to explain why the change is
correct without help. Write the PR description yourself, in your own words; if English is not your
first language, translation tools are fine. Reviews are done by people with limited time, and a PR
whose author understands it less well than the reviewer does will be closed.

## Security

Do not open a public issue for a suspected vulnerability. Use GitHub's private vulnerability
reporting on this repository, under the Security tab. See [SECURITY.md](SECURITY.md).
