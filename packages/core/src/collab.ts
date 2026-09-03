// Live-collaboration provider: drives a ScriptorView as a real loro peer over a
// collaboration websocket.
//
// Wire: a binary websocket whose URL the host supplies (`connect()`), with an
// optional single-use ticket carried as a `tkt.<...>` subprotocol alongside the
// protocol name. Binary frames are raw loro blobs (the server sends a full
// snapshot on join, then peer update deltas / re-sync snapshots); Text frames
// are ephemeral cursor presence (JSON).
//
// Responsibilities: merge remote doc updates into the view (caret-preserving),
// ship local edits as loro deltas, relay this peer's cursor + render others',
// and reconnect with backoff (re-fetching a fresh single-use ticket each time).

import type { RemoteCursor } from './types';
import type { ScriptorView } from './view';

export interface CollabConnectInfo {
  /** Absolute ws(s) URL to connect to. */
  url: string;
  /** A fresh, single-use ticket, sent as the `tkt.<ticket>` subprotocol. Omit it
   *  for a relay that authenticates some other way (or not at all). */
  ticket?: string;
}

export type CollabStatus = 'connecting' | 'online' | 'offline';

export interface CollabProviderOptions {
  view: ScriptorView;
  /** Fetch a fresh ws URL + single-use ticket. Called on every (re)connect, so
   *  each socket gets an unspent ticket. */
  connect: () => Promise<CollabConnectInfo>;
  /** Connection-status callback (for a presence/status chip). */
  onStatus?: (status: CollabStatus) => void;
  /** Debounce (ms) for coalescing local edits into one delta send (and cursor
   *  presence). Default 120. */
  debounceMs?: number;
  /** Scroll the view to follow an AGENT collaborator as it edits: when an agent
   *  presence frame with `state: 'typing'` lands, scroll its position into view
   *  so the user watches the redlines appear even off the visible page. Default
   *  false (a human collaborator's caret never steals the local scroll). */
  followAgent?: boolean;
  /** Websocket subprotocol to offer, which the server must echo. Defaults to
   *  [`DEFAULT_SUBPROTOCOL`]. Set this when talking to a relay that negotiates
   *  its own protocol name. */
  subprotocol?: string;
}

/** The subprotocol offered (alongside the ticket) unless the host overrides it. */
export const DEFAULT_SUBPROTOCOL = 'scriptor-collab.v1';
const MAX_BACKOFF_MS = 10_000;

function b64encode(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function b64decode(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function versionsEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export class CollabProvider {
  private readonly view: ScriptorView;
  private readonly getConn: () => Promise<CollabConnectInfo>;
  private readonly onStatus?: (s: CollabStatus) => void;
  private readonly debounceMs: number;
  private readonly followAgent: boolean;
  private readonly subprotocol: string;

  private ws: WebSocket | null = null;
  /** Got the first snapshot ever (so later frames merge instead of replace). */
  private loaded = false;
  /** The doc version we've shipped to the server up to. Advances only on send,
   *  so a local edit is never lost; recently-received remote ops may be re-sent
   *  on the next local edit (harmless - the server merges idempotently). */
  private lastSent: Uint8Array | null = null;
  /** After a reconnect, flush local-unsent edits once the re-sync snapshot has
   *  merged (the socket dropped before they were sent). */
  private resyncFlush = false;
  private remote = new Map<string, RemoteCursor>();
  private destroyed = false;
  private backoff = 500;
  private changeTimer: ReturnType<typeof setTimeout> | null = null;
  private selTimer: ReturnType<typeof setTimeout> | null = null;
  private unsubView: (() => void) | null = null;
  /** True only while we are applying a remote update, so the view's change
   *  notification (which fires for both local edits and remote merges) is not
   *  mistaken for a local edit and echoed back to the server. */
  private applyingRemote = false;

  constructor(opts: CollabProviderOptions) {
    this.view = opts.view;
    this.getConn = opts.connect;
    this.onStatus = opts.onStatus;
    this.debounceMs = opts.debounceMs ?? 120;
    this.followAgent = opts.followAgent ?? false;
    this.subprotocol = opts.subprotocol ?? DEFAULT_SUBPROTOCOL;
  }

  /** Begin connecting + subscribe to the view's change bus. */
  start(): void {
    if (this.destroyed) return;
    // One bus covers local edits, selection moves, and command-driven state
    // changes; the applyingRemote guard filters out remote-merge notifications.
    this.unsubView = this.view.addListener(() => this.onLocalNotify());
    void this.open();
  }

  /** The view changed locally (edit or caret move): ship the edit delta and our
   *  cursor. Skipped while we are merging a remote update (not our change). */
  private onLocalNotify(): void {
    if (this.applyingRemote) return;
    if (this.changeTimer) clearTimeout(this.changeTimer);
    this.changeTimer = setTimeout(() => this.flushLocal(), this.debounceMs);
    if (this.selTimer) clearTimeout(this.selTimer);
    this.selTimer = setTimeout(() => this.sendPresence(), this.debounceMs);
  }

  /** Tear down: stop reconnecting, unsubscribe, close the socket. */
  destroy(): void {
    this.destroyed = true;
    if (this.unsubView) {
      this.unsubView();
      this.unsubView = null;
    }
    if (this.changeTimer) clearTimeout(this.changeTimer);
    if (this.selTimer) clearTimeout(this.selTimer);
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        /* ignore */
      }
      this.ws = null;
    }
  }

  private async open(): Promise<void> {
    if (this.destroyed) return;
    this.onStatus?.('connecting');
    let info: CollabConnectInfo;
    try {
      info = await this.getConn();
    } catch {
      this.scheduleReconnect();
      return;
    }
    if (this.destroyed) return;

    let ws: WebSocket;
    try {
      // A relay that issues no ticket gets the bare protocol name, so the ticket stays optional.
      const protocols = info.ticket
        ? [this.subprotocol, `tkt.${info.ticket}`]
        : [this.subprotocol];
      ws = new WebSocket(info.url, protocols);
    } catch {
      this.scheduleReconnect();
      return;
    }
    ws.binaryType = 'arraybuffer';
    this.ws = ws;
    // A reconnect (we've loaded before) must flush local-unsent edits once the
    // re-sync snapshot lands.
    this.resyncFlush = this.loaded;

    ws.onopen = () => {
      this.backoff = 500;
      this.onStatus?.('online');
    };
    ws.onmessage = (ev) => this.onMessage(ev);
    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      this.onStatus?.('offline');
      this.scheduleReconnect();
    };
    ws.onerror = () => {
      try {
        ws.close();
      } catch {
        /* onclose handles reconnect */
      }
    };
  }

  private onMessage(ev: MessageEvent): void {
    if (typeof ev.data === 'string') {
      this.onPresence(ev.data);
      return;
    }
    const bytes = new Uint8Array(ev.data as ArrayBuffer);
    // Guard the view mutations so the change notification they trigger isn't
    // mistaken for a local edit (which would echo the merge back to the server).
    this.applyingRemote = true;
    try {
      if (!this.loaded) {
        // First snapshot ever: it is the authoritative content.
        this.view.loadSnapshot(bytes);
        this.loaded = true;
        this.lastSent = this.view.oplogVersion();
        return;
      }
      // A peer delta or a re-sync snapshot: merge it (caret-preserving).
      this.view.applyRemoteUpdate(bytes);
    } finally {
      this.applyingRemote = false;
    }
    if (this.resyncFlush) {
      // The socket dropped before some local edits were sent; ship them now.
      this.resyncFlush = false;
      this.flushLocal();
    }
  }

  private flushLocal(): void {
    if (this.changeTimer) {
      clearTimeout(this.changeTimer);
      this.changeTimer = null;
    }
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.loaded || !this.lastSent) return;
    let delta: Uint8Array;
    try {
      delta = this.view.exportUpdatesSince(this.lastSent);
    } catch {
      return;
    }
    const after = this.view.oplogVersion();
    if (versionsEqual(after, this.lastSent)) return; // nothing new
    // Copy into a fresh ArrayBuffer-backed view so the type is BufferSource
    // (the wasm Uint8Array is generic over ArrayBufferLike).
    ws.send(new Uint8Array(delta));
    this.lastSent = after;
  }

  private sendPresence(): void {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    const anchor = this.view.localCaretAnchor();
    const payload = anchor ? { anchor: b64encode(anchor) } : {};
    ws.send(JSON.stringify(payload));
  }

  private onPresence(text: string): void {
    let msg: {
      t?: string;
      peer?: number | string;
      name?: string;
      color?: string;
      anchor?: string;
      state?: 'thinking' | 'typing';
    };
    try {
      msg = JSON.parse(text);
    } catch {
      return;
    }
    const peer = String(msg.peer ?? '');
    if (!peer) return;
    if (msg.t === 'leave' || typeof msg.anchor !== 'string') {
      this.remote.delete(peer);
    } else {
      const anchor = b64decode(msg.anchor);
      this.remote.set(peer, {
        peer,
        name: String(msg.name ?? ''),
        color: String(msg.color ?? '#2563eb'),
        anchor,
        ...(msg.state ? { state: msg.state } : {}),
      });
      // Follow an agent as it edits: scroll its caret into view when it is
      // actively typing (a redline landing), so changes off the visible page
      // come into view. Only `typing` (an edit), not `thinking` (a read scan),
      // so the viewport isn't yanked around while the agent is just reading.
      if (this.followAgent && msg.state === 'typing') {
        this.view.scrollToAnchor(anchor);
      }
    }
    this.view.setRemoteCursors([...this.remote.values()]);
  }

  private scheduleReconnect(): void {
    if (this.destroyed) return;
    const ms = this.backoff;
    this.backoff = Math.min(this.backoff * 2, MAX_BACKOFF_MS);
    setTimeout(() => {
      if (!this.destroyed) void this.open();
    }, ms);
  }
}
