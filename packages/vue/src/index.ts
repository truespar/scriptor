import {
  defineComponent,
  h,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
  type PropType,
} from 'vue';
import {
  CollabProvider,
  ScriptorView,
  type CollabProviderOptions,
  type CollabStatus,
  type ScriptorMode,
  type Selection,
  type TrackDisplay,
} from '@truespar/scriptor-core';

/** Collaboration config for the Vue wrapper: the provider options minus the
 *  `view` (which the component owns). Pass it to turn the editor into a live
 *  loro peer over the host collaboration websocket; when present, the
 *  document loads from the server's join snapshot, not the `docx` prop. */
export type ScriptorCollab = Omit<CollabProviderOptions, 'view'>;

/** Vue wrapper around the headless [`ScriptorView`]. Emits `change`,
 *  `selectionChange`, `ready`, and (when collaborating) `collabStatus`. */
export const ScriptorDoc = defineComponent({
  name: 'ScriptorDoc',
  props: {
    docx: { type: Object as PropType<Uint8Array>, default: undefined },
    mode: { type: String as PropType<ScriptorMode>, default: 'read' },
    gutter: { type: String, default: undefined },
    selectable: { type: Boolean, default: true },
    /** Live-collaboration config; when set, the doc syncs over the websocket. */
    collab: { type: Object as PropType<ScriptorCollab>, default: undefined },
  },
  emits: {
    change: () => true,
    selectionChange: (_s: Selection | null) => true,
    ready: () => true,
    collabStatus: (_s: CollabStatus) => true,
    /** The view's chrome-visible state moved - zoom, page count, page, words.
     *  A host drawing its OWN toolbar (rather than using `Workspace`'s status
     *  bar) needs this: the core binds Ctrl/Cmd +/-/0 and Ctrl+wheel itself, so
     *  a zoom readout that only updated when the host called `setZoom` would go
     *  stale the first time the user reached for the keyboard. */
    stateChange: () => true,
  },
  setup(props, { emit, expose }) {
    const el = ref<HTMLDivElement | null>(null);
    let view: ScriptorView | null = null;
    let provider: CollabProvider | null = null;
    let unlisten: (() => void) | null = null;

    onMounted(async () => {
      if (!el.value) return;
      view = await ScriptorView.create(el.value, {
        mode: props.mode,
        gutter: props.gutter,
        selectable: props.selectable,
        onChange: () => emit('change'),
        onSelectionChange: (s) => emit('selectionChange', s),
        onReady: () => emit('ready'),
      });
      unlisten = view.addListener(() => emit('stateChange'));

      if (props.collab) {
        // Collaborative: the document loads from the server's join snapshot; the
        // local `docx` prop is ignored (the Live Document is authoritative).
        provider = new CollabProvider({
          view,
          ...props.collab,
          onStatus: (s) => {
            emit('collabStatus', s);
            props.collab?.onStatus?.(s);
          },
        });
        provider.start();
      } else if (props.docx) {
        view.loadDocx(props.docx);
      }
    });

    watch(
      () => props.mode,
      (m) => view?.setMode(m),
    );
    watch(
      () => props.docx,
      (d) => {
        // In collab mode the server is the source of truth; ignore docx swaps.
        if (d && !props.collab) view?.loadDocx(d);
      },
    );

    onBeforeUnmount(() => {
      unlisten?.();
      unlisten = null;
      provider?.destroy();
      provider = null;
      view?.destroy();
      view = null;
    });

    // The surface a host with its own chrome drives the view through. Zoom is
    // here rather than as a prop because the core owns it too (keyboard +
    // Ctrl-wheel): a prop would make the host the single source of truth for a
    // value the user can change behind its back. Read it back on `stateChange`.
    expose({
      loadDocx: (bytes: Uint8Array) => view?.loadDocx(bytes),
      toDocumentXml: () => view?.toDocumentXml() ?? '',
      /** Set the zoom factor (1 = 100%); the core clamps to 25%..400%. */
      setZoom: (factor: number) => view?.setZoom(factor),
      /** The live zoom factor, whoever last changed it. */
      getZoom: () => view?.zoomLevel ?? 1,
      /** Pages in the current layout (0 before the first render). */
      pageCount: () => view?.pageCount() ?? 0,
      /** Word's "Display for Review": how tracked changes are shown. The engine
       *  defaults to `'all'`, so a document WITH revisions shows them without
       *  the host asking - this is for offering the other three views. */
      trackDisplay: () => view?.trackDisplayMode ?? 'all',
      setTrackDisplay: (mode: TrackDisplay) => view?.setTrackDisplay(mode),
      /** Everyone who authored a tracked change or comment. Empty means the
       *  document simply has no revisions - which is how a host decides whether
       *  a review control is worth showing at all. */
      reviewers: () => view?.reviewers() ?? [],
    });

    return () => h('div', { ref: el });
  },
});

export type { CollabConnectInfo, CollabStatus } from '@truespar/scriptor-core';
