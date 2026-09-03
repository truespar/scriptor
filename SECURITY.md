# Security policy

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository (the Security tab, then "Report a
vulnerability"). It keeps the report private until a fix is released and needs no email exchange
to get started. Please do not open a public issue for a suspected vulnerability.

We will acknowledge the report, say whether we consider it in scope, and keep you updated while a
fix is prepared. If you want credit in the release notes, tell us which name to use.

## Scope

Scriptor parses `.docx` files, which are attacker-supplied ZIP archives of XML and binary media.
That is the main attack surface:

- A crafted document that panics, hangs, or exhausts memory in the parser, the CRDT import, or the
  layout engine. In the browser, a Rust panic aborts the wasm instance and takes the editor down
  until the page is reloaded, so a reliable panic is a denial of service.
- A document that escapes the sandbox by causing a network request, reading a local file, or
  executing script through the editor.
- Anything in the collaboration relay (`scriptor-server`) that lets a client reach a document it
  should not, or corrupt another room's state.

A reproducer document helps more than a description, and a fuzzer-minimised one is best. If the
document is confidential, a hand-built minimal file that triggers the same crash works.

## Out of scope

`scriptor-server` has no authentication or authorization. The websocket endpoint at
`/doc/{id}` upgrades any client that knows the document id. This is by design: the relay is meant
to be embedded as a library or run behind your own authenticating proxy, so its lack of auth is
not a vulnerability report. Deployed directly on a public port, every document in it is public,
which is a deployment decision rather than a bug.

Reports that depend on a modified build, on a document the victim would have to construct
themselves, or on an already-compromised machine are also out of scope.

## Supported versions

Scriptor is pre-1.0 and is developed on the main line, where fixes go. There are no maintained
release branches yet.
