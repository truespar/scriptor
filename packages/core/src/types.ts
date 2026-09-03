// The public shape of the view: what a host passes in, and what it gets back.
//
// These are the types `@truespar/scriptor-core` exports. They are declared apart from the view so
// a consumer can import them without pulling the implementation, and so adding an option is a
// one-file change.

/** How the view behaves: a passive viewer, or an editor. */
export type ScriptorMode = 'read' | 'edit';

/**
 * How tracked changes (insertions / deletions) are displayed - mirrors Word's "Display for Review".
 * - `all` (default): insertions underlined, deletions struck through, both in per-author colours.
 * - `simple`: clean text (deletions hidden, insertions as normal text) plus a margin change-bar
 *   beside every changed paragraph - Word's "Simple Markup". Same text flow as `none`.
 * - `none`: the "Final" / accept-all view (deletions hidden, no change-bar).
 * - `original`: insertions hidden (the pre-change document).
 *
 * The non-`all` modes are render/preview only: the caret geometry still indexes the full All-Markup
 * text, so edit in `all`.
 */
export type TrackDisplay = 'all' | 'simple' | 'none' | 'original';

/** A caret position: a codepoint offset within a paragraph. */
export interface CaretPos {
  para: number;
  off: number;
}

/** A selection: anchor (where it started) to focus (where it is now). Collapsed = a caret. */
export interface Selection {
  anchor: CaretPos;
  focus: CaretPos;
}

/** A remote collaborator's caret, for presence rendering. `anchor` is an opaque,
 *  edit-stable loro cursor (from another peer's `localCaretAnchor()`); the view
 *  resolves it against the local document at draw time, so a remote caret stays
 *  on its character even as local edits shift offsets. */
export interface RemoteCursor {
  /** Stable peer id (so updates replace, not duplicate). */
  peer: string;
  /** Display name for the caret's name tag. */
  name: string;
  /** CSS color for the caret bar + tag. */
  color: string;
  /** Opaque anchor bytes from the peer's `localCaretAnchor()`. */
  anchor: Uint8Array;
  /** When set, the caret is an agent at work: render a "presence chip" (avatar +
   *  "{name} is thinking…/typing…" with animated dots) at the caret instead of a
   *  plain canvas name-tag. Absent for ordinary human collaborators. */
  state?: 'thinking' | 'typing';
}

/** An in-progress picture drag on the overlay. `mode` is 'resize' (a handle is grabbed) or 'move'
 *  (the body of a floating picture is grabbed). `handle` is the grabbed handle name (resize only,
 *  one of nw/n/ne/w/e/sw/s/se). `startX/Y` is the pointer-down canvas point; `rect*` is the picture's
 *  canvas rect at grab time; `aspect` is w/h for Shift aspect-lock. `moved` guards a click vs a drag. */
export interface ImageDrag {
  id: bigint;
  mode: 'resize' | 'move' | 'crop';
  handle: string;
  startX: number;
  startY: number;
  rectX: number;
  rectY: number;
  rectW: number;
  rectH: number;
  aspect: number;
  moved: boolean;
  // The live (proposed) rect, updated each pointermove and drawn as the drag preview; committed to
  // the model on drop.
  curX: number;
  curY: number;
  curW: number;
  curH: number;
}



/** A comment as returned by the engine's `listComments()` (a JSON array). `parent` is the id of the
 *  comment this replies to (`null` for a top-level comment); `para`/`off` is the anchor caret
 *  (namespaced), `-1` when un-anchored. */
export interface CommentJson {
  id: number;
  author: string;
  initials: string;
  date: string;
  text: string;
  parent: number | null;
  resolved: boolean;
  para: number;
  off: number;
}

/** One row in the reviewing pane: a tracked change (`ins`/`del`/`fmt`) or a top-level `comment`,
 *  with the changed/comment text and its anchor caret (`para` namespaced, `off`). `id` is the
 *  revision id (changes) or comment id; `resolved` is set for comments. */
export interface ReviewItem {
  kind:
    | 'ins'
    | 'del'
    | 'fmt'
    | 'movefrom'
    | 'moveto'
    | 'rowins'
    | 'rowdel'
    | 'colins'
    | 'coldel'
    | 'tblprop'
    | 'rowprop'
    | 'cellprop'
    | 'comment';
  id: number;
  author: string;
  date: string;
  text: string;
  para: number;
  off: number;
  resolved?: boolean;
}

/** One quick style in the Styles gallery, with the resolved formatting to render its live preview.
 *  `size` is half-points (0 = inherit from the base style); `color` is a hex string ('' = inherit). */
export interface StyleGalleryItem {
  id: string;
  name: string;
  size: number;
  bold: boolean;
  italic: boolean;
  color: string;
  font: string;
}

/** The resolved definition of a paragraph style, for prefilling the Modify-Style dialog. `size` is
 *  half-points (0 = inherit); `color`/`font` are '' = inherit; `lineSpacing` is 240ths (0 = inherit);
 *  `spaceBefore`/`spaceAfter` are twips (-1 = inherit). */
export interface ResolvedStyleProps {
  size: number;
  bold: boolean;
  italic: boolean;
  color: string;
  font: string;
  lineSpacing: number;
  /** Line-spacing rule: 'auto' (lineSpacing is 240ths of a line) | 'exact' (lineSpacing is twips). */
  lineRule: string;
  spaceBefore: number;
  spaceAfter: number;
  /** Paragraph alignment: 'left' | 'center' | 'right' | 'justify' ('' = inherit). */
  align: string;
}

/** A style-definition edit (Modify-Style). Every field is optional; an omitted field is left
 *  unchanged (per-field merge over the style's existing definition). */
export interface StyleEdit {
  size?: number;
  bold?: boolean;
  italic?: boolean;
  color?: string;
  font?: string;
  lineSpacing?: number;
  /** 'auto' (lineSpacing is 240ths) | 'exact' (lineSpacing is twips). Defaults to 'auto'. */
  lineRule?: string;
  spaceBefore?: number;
  spaceAfter?: number;
  /** 'left' | 'center' | 'right' | 'justify'. */
  align?: string;
}


/** Options for the in-editor single-line input dialog (`ScriptorView.promptInput`). */
export interface InputDialogOptions {
  /** Header text (e.g. `'Insert hyperlink'`). */
  title: string;
  /** Pre-filled value; the text is selected on open so typing replaces it. */
  value?: string;
  /** Greyed placeholder shown when the field is empty. */
  placeholder?: string;
  /** A small explanatory line under the field. */
  hint?: string;
  /** Primary-button label (default `'OK'`). */
  okLabel?: string;
}

/** One entry in the right-click context menu. A separator is `{ separator: true }` (or a `label`
 *  of `-`/`—` with no `onClick`); anything else is a clickable command. Disabled items render
 *  greyed and do nothing. Used both for the built-in items handed to [`ScriptorOptions.onContextMenu`]
 *  and for the items an integrator returns from it. */
export interface ScriptorContextMenuItem {
  /** Display text. Omit (or use `-`/`—`) for a separator. */
  label?: string;
  /** Command to run when clicked. Omit for a separator or a header. */
  onClick?: () => void;
  /** Render greyed and non-interactive (e.g. Cut with no selection). */
  disabled?: boolean;
  /** Draw a divider line instead of a command. */
  separator?: boolean;
}

/** What the pointer is over when the context menu opens - passed to [`ScriptorOptions.onContextMenu`]
 *  so an integrator can add/remove items conditionally (e.g. a "Define" item only when text is
 *  selected, or an app command only inside a table). */
export interface ScriptorContextMenuContext {
  /** Paragraph (flat index) + codepoint offset under the pointer. */
  para: number;
  offset: number;
  /** True when a non-empty selection exists - i.e. when there is text to copy. */
  hasSelection: boolean;
  /** The selected text, across as many paragraphs as it spans (newline-joined); empty when the
   *  selection is collapsed. */
  selectionText: string;
  /** Hyperlink target under the pointer (`''` when none). */
  linkTarget: string;
  /** True when the pointer is on a tracked change. */
  onTrackedChange: boolean;
  /** True when the pointer is in a table cell. */
  inTable: boolean;
  /** True when the pointer is on a picture. */
  onImage: boolean;
  /** Screen coordinates of the click (for positioning custom UI). */
  clientX: number;
  clientY: number;
}

/** Configuration for a [`ScriptorView`]. All fields optional; sensible defaults are applied. */
export interface ScriptorOptions {
  /** `'read'` (default) renders + lets the user select; `'edit'` adds the caret, keyboard, and edits. */
  mode?: ScriptorMode;
  /** Current author for tracked changes (stamped as `w:author`, shown in the hover tooltip). The
   *  `id` is a stable identity for the audit trail. Default `{ id: 'local', name: 'You' }`. */
  author?: { id: string; name: string };
  /** Device-pixel ratio to render at. Defaults to `window.devicePixelRatio` (min 1) for crisp text. */
  scale?: number;
  /** Color of the gutter between page sheets. Default `#d8dce4`. */
  gutter?: string;
  /** Allow click/drag selection in read mode (e.g. for copy). Default `true`. */
  selectable?: boolean;
  /** Show the built-in right-click table menu (insert/delete row & column) when the caret is in a
   *  table cell, in edit mode. Default `true`. Set `false` to supply your own table chrome. */
  tableMenu?: boolean;
  /** Called after an edit changes the document (edit mode only). */
  onChange?: () => void;
  /** Called whenever the selection or caret moves (collapsed selection = caret). */
  onSelectionChange?: (selection: Selection | null) => void;
  /** Called once the engine is ready and the first render has happened. */
  onReady?: () => void;
  /** Called when the user invokes Save (the QAT button or Ctrl/Cmd+S), with the serialized `.docx`
   *  bytes. The host decides where they go (download, upload, IndexedDB…). When omitted, Save falls
   *  back to triggering a `.docx` download in the browser. */
  onSave?: (bytes: Uint8Array) => void;
  /** Customize the right-click context menu. Called with what the pointer is over (`ctx`) and the
   *  built-in items Scriptor would show (`defaults`, already context-adapted: clipboard, link, change,
   *  table, picture, Select All). Return an item list to display (splice your own commands into
   *  `defaults`, or build a fresh list), an empty array to show nothing, or `null` to fall through to
   *  the browser's native menu. Returning `undefined` (or omitting the option) uses `defaults`. */
  onContextMenu?: (
    ctx: ScriptorContextMenuContext,
    defaults: ScriptorContextMenuItem[],
  ) => ScriptorContextMenuItem[] | null | undefined;
}

/** A change to mark on the page - a translucent band behind paragraph `para`, coloured by `kind`
 *  (the side-by-side comparison view sets these; `active` is the one currently navigated to). */
export interface ChangeMarker {
  para: number;
  /** `insert` (cool/blue), `delete` (warm/red), or `edited` (amber) - matching the redline palette. */
  kind: 'insert' | 'delete' | 'edited';
  /** The currently-focused change: painted stronger + a left accent bar. */
  active?: boolean;
}
