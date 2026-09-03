# Building and using Scriptor

How to build the workspace and consume each surface: the browser editor, the headless
Rust agent path, document comparison, and the collaboration relay.

## Build

Prerequisites: Rust (stable, includes the `wasm32-unknown-unknown` target via rustup),
[wasm-pack](https://rustwasm.github.io/wasm-pack/), Node >= 22, pnpm.

```sh
cargo build --workspace          # the Rust engine, server, CLI
pnpm install
pnpm -r build                    # builds the wasm engine (wasm-pack) first, then every package
pnpm --filter @scriptor/playground dev   # the demo editor on :5174
```

On Windows, run cargo commands from the environment set up by `scripts\dev-shell.ps1`
(see the README's Windows note) - this matters for wasm-pack too, because build scripts
compile for the host.

## Embed the editor (browser)

### The full Word-style editor: `Workspace`

`@truespar/scriptor-ui` ships the complete chrome (ribbon, rulers, reviewing pane,
compare dialog, status bar) around the headless core - the playground is exactly this:

```ts
import { Workspace } from '@truespar/scriptor-ui';

const ws = await Workspace.create(document.getElementById('editor')!, {
  mode: 'edit',
  author: { id: 'u42', name: 'Ada Reviewer' },   // stamped on tracked changes
  onSave: (bytes) => upload(bytes),              // Quick Access Toolbar Save / Ctrl+S
  // showSave: false,                            // hide Save when the host autosaves
  // Optional: wire your review model to annotate comparison changes (materiality/risks).
  // compareAnnotate: async (result) => annotations,
  // Optional: offer stored versions in the Compare dialog's picker.
  // compareVersions: () => versions.map(v => ({ id, label, load: () => bytes })),
});

ws.loadDocx(new Uint8Array(await file.arrayBuffer()));
const saved: Uint8Array = ws.view.toDocx();   // a .docx that opens in Word
```

### The headless core: `ScriptorView`

`@truespar/scriptor-core` is the canvas editor with no chrome - bring your own UI:

```ts
import { ScriptorView } from '@truespar/scriptor-core';

const view = await ScriptorView.create(mount, {
  mode: 'edit',                        // 'read' for a viewer
  onChange: () => markDirty(),
});

view.loadDocx(bytes);
view.setAuthor('u42', 'Ada Reviewer'); // stamped as w:author on tracked changes
view.setTrackChanges(true);            // typing now lands as w:ins / deletes as w:del
// ... the user edits; enumerate review state with view.reviewItems() ...
view.acceptAll();
const out = view.toDocx();
```

Display modes (`setTrackDisplay('all' | 'simple' | 'none' | 'original')`) follow Word's
"Display for Review". Edit in `'all'`; the other modes are render-only.

### Framework wrappers

All four are thin layers over `ScriptorView`:

```vue
<!-- Vue -->
<ScriptorDoc :docx="bytes" mode="edit" @change="onChange" />
```

```tsx
// React
<ScriptorDoc docx={bytes} mode="edit" ref={handle} />   // handle.current?.loadDocx(...)
```

```svelte
<!-- Svelte (action) -->
<div use:scriptor={{ docx: bytes, mode: 'edit' }} />
```

```html
<!-- Framework-free custom element -->
<scriptor-doc mode="edit" src="/contract.docx"></scriptor-doc>
<script>document.querySelector('scriptor-doc').loadDocx(bytes)</script>
```

## Redline headlessly from Rust (the agent path)

There are two ways to give an agent (or any service) the ability to redline documents, and they
are equally supported. Both produce **identical output**, since every mutation is the same
tracked-suggestion primitive on the same CRDT, stamped with `w:author`. The choice is only about
which side owns the safety checks:

- **`AgentPeer`** (`scriptor-client`) - the packaged agent surface, with safety rails
  built in. Choose it for a standalone tool or service, or when you do not already have
  governance/audit infrastructure of your own.
- **Direct `CollabDoc` + the room actor** - the embedded-host shape. Choose it when
  your platform already owns identity, persistence, policy, and its own agent tool
  layer, and Scriptor should be an engine inside it, not a framework around it.

### Shape 1: `AgentPeer` - the packaged surface

The attributed, headless participant - an agent edits without any browser or Word
instance:

```rust
use scriptor_client::AgentPeer;

let bytes = std::fs::read("contract.docx")?;
let agent = AgentPeer::from_docx_bytes("Review Agent", &bytes)?;

// Perceive: quote-based addressing; anchors are edit-stable (never raw offsets).
let hits = agent.find("shall indemnify", true)?;
let hit = hits.first().expect("clause present");

// Propose: lands as a native tracked change (w:del + w:ins) attributed to the agent.
agent.propose_replace(&hit.anchor, "may indemnify",
    "2026-07-25T12:00:00Z", "soften the obligation")?;

// Or annotate instead of editing:
agent.add_comment(&hit.anchor, "Consider a mutual carve-out here.",
    "2026-07-25T12:00:00Z")?;

std::fs::write("contract-redlined.docx", agent.to_docx_bytes()?)?;
```

The output opens in Word with "Review Agent" as the named reviewer; a human accepts or
rejects each change natively (or programmatically: `accept_change(id)` / `accept_all()`).

Beyond the basics:

- **Perception**: `outline(preview_chars, offset, max_nodes)` returns a token-budgeted
  document outline with stable node ids; `read_node(id)` reads one node verbatim
  (read-before-write); `list_changes()` / `comments()` enumerate review state.
- **Atomic batches**: build a `Proposal { base_revision, title, ops }` and
  `submit_proposal` it - validate-first, all-or-nothing, with optimistic concurrency
  (`Stale` if the document moved since the agent read it).
- **Live participation**: `AgentPeer::join(author, snapshot)` makes the agent a real
  CRDT peer in a collaborative session - it exchanges the same opaque loro update bytes
  as human clients (`export()` / `merge()`).
- **Governance and audit**: `add_policy(...)` vetoes actions before they touch the
  document ("this agent may comment but not delete"); `add_sink(...)` receives every
  applied action for an audit log; `on_behalf_of(principal)` records delegation.
- **Non-Rust agents**: every capability has a JSON form (`scriptor_client::wire`) - an
  integrator exposes them as RPC/MCP tools (e.g. `document_read` / `document_propose_edit` /
  `document_comment`) and forwards the DTOs; anchors travel as opaque tokens the agent
  echoes back.

**Why this shape:** the rails come for free. Anchors are edit-stable (a concurrent human
edit cannot silently misplace an agent's change), proposals are validate-first and
all-or-nothing with optimistic concurrency, policies veto actions before they touch the
document, sinks capture an audit trail, and the wire contract makes the whole surface
reachable from any language. If you build on `CollabDoc` directly, each of those is
yours to re-implement.

### Shape 2: direct `CollabDoc` + the room actor - the embedded-host pattern

A host platform that already has identity, tenancy, persistence, and audit can skip
`AgentPeer` and drive the engine directly, defining its own tool surface (its own
addressing model, its own tool names) over the same tracked primitives:

```rust
use scriptor_crdt::CollabDoc;
use scriptor_server::room::SERVER_CLIENT;

// Headless: reopen from your own store, mutate, persist.
let doc = CollabDoc::new();
for blob in store.load(doc_id)? {
    doc.merge(&blob)?;                     // snapshot + op-log blobs, in order
}
doc.suggest_deletion_multi(sp, so, ep, eo, "Review Agent", date, audit)?;
doc.suggest_insertion(sp, so, "may indemnify", "Review Agent", date, audit)?;
store.compact(doc_id, &doc.snapshot()?)?;

// Live: when a collaborative session is open for this document, apply through its
// room instead - connected editors watch the redline appear, and there is no second
// copy to reconcile (SERVER_CLIENT marks the write as server-side, so every client
// receives the delta).
room.apply(SERVER_CLIENT, move |doc| {
    doc.suggest_insertion(para, off, " (as amended)", "Review Agent", date, audit)
})
.await?;
```

**Why this shape:** one authority per document. The live-or-headless routing above is
the key property - all writers go through the room when one exists, so nothing compacts
the op-log behind a live session's back, and an agent's redline lands in front of
connected users in real time. The host also gets full control of the ergonomics: it can
address by plain `(paragraph, offset)` indices from its own read tool, or hand out
opaque `AnchorRange` tokens and drift-correct them at apply time with
`doc.resolve_range_multi(&range)`; it stamps `w:author` from its own identity system
(display name into the document, internal actor id into the `audit` string); and it
reaches every engine primitive directly, including ones the packaged surface wraps
more opinionatedly.

**The cost** is symmetric: read-before-write freshness checks, batch atomicity, veto
policies, and audit emission are now the host's responsibility - `AgentPeer` is
effectively the reference implementation of those rails, worth reading even if you
bypass it.

One practical detail for this shape: a loro snapshot carries the document *body*, not
the whole OPC package. To materialize a Word-openable file from stored state,
re-attach the original package parts first:

```rust
doc.attach_source_parts_from_docx(&origin_docx_bytes)?;  // theme, settings, fonts, ...
let bytes = doc.to_docx_bytes()?;
```

## Compare two documents (blacklining)

```rust
use scriptor_client::compare_docx;

let result = compare_docx(&original_bytes, &revised_bytes, "Compare")?;
std::fs::write("redline.docx", &result.redline)?;      // a native Word redline
for change in &result.manifest.changes { /* feed to a review model */ }
```

Or from the CLI, with the correctness oracle (accept-all reproduces B, reject-all
reproduces A):

```sh
cargo run -p scriptor-cli -- compare original.docx revised.docx out.docx --manifest changes.json
cargo run -p scriptor-cli -- compare original.docx revised.docx --check
```

In the browser, the same engine runs in a Web Worker: `compareDocuments` in
`@truespar/scriptor-core`, or the Review ribbon's Compare dialog in the `Workspace`.

## Real-time collaboration

`scriptor-server` is the reference relay: rooms keyed by document id, snapshot-on-join,
then raw binary loro update frames both ways. Convergence is the CRDT's job, so the
relay never interprets document content. It is also consumable **as a library** - a host
daemon can `use scriptor_server::room::spawn_room` and implement the
`scriptor_server::persistence::Persistence` trait over its own store, embedding the room
actor behind its own authenticated endpoint instead of running the binary. This pairs
with the direct-engine agent shape above: the same room actor then serves browser peers
and server-side agent writes through one authority.

```sh
SCRIPTOR_PORT=8091 cargo run -p scriptor-server
# durable persistence:
cargo run -p scriptor-server --features postgres   # with DATABASE_URL set
```

Endpoint: `ws://host:8091/doc/{id}`. A joining client receives the current snapshot as
the first binary frame, then sends/receives loro update deltas. A Rust agent joins the
same room by loading the snapshot (`AgentPeer::join`) and exchanging `export()` bytes.

The browser side is `CollabProvider` (`@truespar/scriptor-core`): give it a `connect`
callback returning `{ url, ticket }` and it keeps the view in sync, ships cursor
presence, and reconnects with backoff (fetching a fresh single-use ticket each time):

```ts
import { CollabProvider } from '@truespar/scriptor-core';

const provider = new CollabProvider({
  view,
  connect: async () => {
    const r = await fetch(`/api/documents/${docId}/collab-ticket`, { method: 'POST' });
    return await r.json();   // { url, ticket }
  },
  onStatus: (s) => statusChip.textContent = s,   // connecting | online | offline
  followAgent: true,   // scroll to watch an agent's redlines appear live
});
```

Note the division of labor: the reference relay does no authentication, does not
negotiate the ticket subprotocol, and forwards **binary document frames only** - a
production deployment fronts it with (or embeds it in) the host platform's gateway,
which authenticates the single-use ticket, selects the websocket subprotocol, relays
the Text-frame cursor-presence channel, and maps its document ids to rooms. The
document wire format either side of that gateway is the same raw loro frames.
