## What this changes

<!-- What the change does, and why. The reasoning matters more than the diff. -->

## How it was verified

<!-- Commands run, or the scenario exercised. For a fidelity change, say which
     real documents you checked the output against, and against which Word. -->

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo deny check`
- [ ] `pnpm typecheck && pnpm lint && pnpm test`
- [ ] The corpus gate (`scripts/corpus-gate.ps1`), if the save path changed

## Checklist

- [ ] Added or updated tests
- [ ] Updated `README.md` / `docs/` if setup or behaviour changed
- [ ] Updated `THIRD-PARTY-NOTICES.md` if dependencies changed
- [ ] A document that round-trips through this change still opens in Word
      without a repair prompt
- [ ] No content is silently dropped on the save path - unmodelled OOXML is
      passed through, not discarded (see [`docs/passthrough.md`](../docs/passthrough.md))
