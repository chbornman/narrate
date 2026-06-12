# UI-ARCHITECTURE.md — P4.2 Component Architecture

Status: design for work packet P4.2. Normative for the frontend build; companions: docs/UI-FEATURESET.md (what), spec/UI.md (philosophy), docs/BUILD-LOOP.md (how staged work lands).

The founder's constraint, restated as the design rule: **reuse is structural**. One typed action registry is the single source of truth for verbs; the keyboard map, all four context menus, the `?` cheatsheet, and tooltips are *renderings* of it. One small primitive kit (each primitive justified by ≥2 consumers, current or named-future) carries all chrome; every surface is a thin composition over it; every future seat (M2a/M2b/M3/M4/M5) is a registry row + a reserved seat in existing chrome, never a new one-off. The established `logic/` (pure, vitest-tested) vs `components/` (thin Svelte) split extends downward: every primitive with non-trivial behavior gets a paired pure controller module that carries its tests. Theming is token-only.

All paths relative to `apps/desktop/` unless prefixed `src-tauri/`.

---

## 1. Directory / module structure (every file named)

```
src/
  app.css                            slimmed: imports theme/tokens.css + base element styles
  App.svelte                         shell: chrome REGIONS (Titlebar, GridSurface, SourceRail,
                                     Inspector, LookSurface, overlays), each gated
                                     {#if !ui.shell.chromeHidden}; ContextMenuHost + ToastHost +
                                     Cheatsheet mounts; drag-folder drop handling; the edge-dwell
                                     hotzone is DELETED (no auto-hide fly-outs, featureset §3)
  lib/
    theme/
      tokens.css                     NEW  all design tokens; surround palettes as
                                          [data-surround="black|dark|middle|light|white"] blocks,
                                          each retuning --surround, --journal-dot, --selection,
                                          --focus, --marquee (D6 contrast tuning); pencil-red
                                          reserved; future full themes = a [data-theme] block
      surround.ts                    NEW  pure: SurroundLevel, cycle order, persistence codec,
                                          per-level contrast table (tested)
    primitives/                      NEW  the kit — app-agnostic, token-driven, no inline styles
      Menu.svelte      menu.ts           popup menu + pure keyboard-nav/typeahead/submenu controller
      Popover.svelte                     anchored floating layer, outside-pointer dismiss,
                                         Esc routed through escape.ts (never self-handled)
      Panel.svelte     panel.ts          PUSH (never overlay) resizable side panel + clamp/persist math
      Sheet.svelte                       quiet dismissable centered sheet (cheatsheet, drop-confirm)
      ToastHost.svelte toast.svelte.ts   sanctioned-toast queue; kind enum IS the §7.5 guardrail
      KeyHint.svelte                     key-chord chip (menus, tooltips, cheatsheet)
      tooltip.ts                         {@attach} key-hinted tooltip; copy resolved from the registry
      EmptyState.svelte                  centered dimmed line + next-action slot
    actions/                         NEW  the single source of truth
      types.ts                       ActionDef, ActionContext, KeyChord, MenuSeat, ModeDef (§3)
      match.ts                       pure (KeyInput, ActionContext, ActionDef[]) → Action | null;
                                     owns the §11 input-suppression rule and scope precedence
      registry.ts                    aggregator only: [...global, ...search, ...rail, ...grid,
                                     ...look, ...inspector] — frozen during parallel work
      defs/global.ts                 Esc · G · Tab(lights-out) · \ (rail) · ?/F1 · / · Ctrl+F · N ·
                                     0–5 · A · F11 · Ctrl+, · Ctrl+Q · F12 · reserved M row
      defs/search.ts                 search-nav · open-result · remove-last-chip (moved, unchanged)
      defs/rail.ts                   rail nav/enter · rail-folder menu rows (Open · Show in file
                                     manager · Rescan)
      defs/grid.ts                   Stage A rows (selection, marquee verbs, T, sizes, Home/End/
                                     PgUp/PgDn, Ctrl+Shift+A, stack collapse/expand, thumb+gutter seats)
      defs/look.ts                   Stage B rows (Z, Ctrl+0/Ctrl+1, Space close/pan, F, R,
                                     backdrop seat; reserved P/E/V band)
      defs/inspector.ts              Stage C rows (I, J, journal row verbs; Metadata/Journal items
                                     seated on the thumb menu)
      menus.ts                       pure (seat, ctx) → MenuModel: queries the registry for defs
                                     with seats.includes(seat) && available(ctx); per-seat order
                                     tables written up front; submenus (Rate ▸ Sort ▸ Size ▸
                                     Stacks ▸ Surround ▸) from parametrized defs; NO key strings,
                                     NO handlers — items carry Actions into ui.perform
      cheatsheet.ts                  pure (ctx) → groups (contract/grid/look/panels/capture/search);
                                     unavailable rows render dimmed, never hidden
      modes.ts                       MODES: ModeDef[] — auto-advance ships; "pencil"/"mic" ids reserved
    logic/                           pure + tested (existing pattern, unchanged home)
      keymap.ts                      KEPT SIGNATURE: dispatch(KeyInput, KeyContext) → Action | null;
                                     KeyContext = ActionContext (new fields optional-with-defaults);
                                     internally match(e, ctx, REGISTRY). Action union extended,
                                     never narrowed. Exactly three test expectations change, each
                                     citing its founder amendment (§6)
      escape.ts                      EXTEND: 12-layer order (§6)
      selection.ts                   EXTEND: active-vs-selected naming (focus ≡ active, documented),
                                     selectNone, Home/End/PageUp/PageDown moves, marquee-merge entry
      scope.ts                       EXTEND: consumes pre-expanded target lists (stack expansion
                                     happens upstream in the grid slice); Look-multi-entry scope
      sort.ts  note.ts               unchanged
      advance.ts                     NEW  afterCommit(ctx) → "look-next" | "grid-next" | null
      segments.ts                    NEW  pure (SegmentInput, MODES) → Segment[]: ingest hairline ·
                                          scope · n-of-m · auto-advance · reserved mic seat ·
                                          reserved query-residue seat (M3)
      sources.ts                     NEW  rail row model extracted from Rail.svelte: SourceSection
                                          providers (folders now; collections/saved-searches M3),
                                          flatten/nav/expand-collapse
      gridlayout.ts                  NEW  integer-column snap (container ÷ N), row math, scroll
                                          anchors/restore, Home/End/Page math          [Stage A]
      marquee.ts                     NEW  classifyDrag(originOnThumb) → "marquee"|"item-drag"(M3);
                                          rect→indices hit-test; additive merge        [Stage A]
      stacks.ts                      NEW  pair by basename+folder → DisplayUnit[]; per-pair +
                                          global collapse; expandTargets() (JPEG then RAW);
                                          flipMember()                                  [Stage A]
      cellinfo.ts                    NEW  T-cycle: none → minimal → annotated-state     [Stage A]
      zoom.ts                        NEW  THE transform (extracted from Look.svelte, anchor bug
                                          fixed): zoomAtPoint keeps the cursor invariant on both
                                          axes incl. letterboxed edges (container-relative points);
                                          fit/100%/clamp; carryOver({mode,scale,centerFrac},
                                          oldDims, newDims) for ←/→ persistence          [Stage B]
      looknav.ts                     NEW  navigation set = entry selection; n-of-m; R member flip;
                                          Space tap-vs-hold resolution (§6)              [Stage B]
      journal.ts                     NEW  display states over JournalEntryDto[]: session grouping,
                                          revision folding ("edited" expand), retracted toggle,
                                          redacted stubs, stroke-stub seat (M2a)         [Stage C]
      metadata.ts                    NEW  EXIF subset → labeled read-only rows; copyable flags [C]
      confirmhold.ts                 NEW  pure hold/type-to-confirm state machine        [Stage C]
    state/
      app.svelte.ts                  composition root: exports `ui` (shell + grid + look +
                                     inspector slices), perform(Action) router, actionContext()
                                     snapshot assembly, cross-slice flows ONLY here: openLook
                                     (entry selection → LookEntry order), leaveLook (scroll-anchor
                                     restore), goHome (G), auto-advance wiring, scope reporting
      shell.svelte.ts                NEW  chromeHidden, railOpen/railFocused, cheatsheetOpen,
                                          contextMenu host state, surround, fullscreen, note,
                                          scope/pulse/ingest, popover
      grid.svelte.ts                 NEW  folder/items/sort/thumbStep/selection/stacks/cellInfo/
                                          scroll anchors; selectionTargets (stack-expanded);
                                          activeHash; advanceActive()                    [Stage A]
      look.svelte.ts                 NEW  LookEntry order/index/member-flips/zoomSession/filmstrip;
                                          next()                                          [Stage B]
      inspector.svelte.ts            NEW  open/tab, journal entries, metadata, inline-correct +
                                          retract/redact flow state, toast triggers       [Stage C]
      prefs.ts                       EXTEND (all keys up front): surround, autoAdvance, cellInfo,
                                     railWidth/open, inspectorWidth, stackGlobal, filmstrip kept
    types/
      dto.ts                         EXTEND: JournalEntryDto, ImageMetadataDto, RedactReportDto
                                     (offlinePending volumes), ImagePathsDto
      display.ts                     NEW  DisplayUnit { primary: GridItem; alt: GridItem | null }
                                     and LookEntry { display: hash; alt: hash | null } — the
                                     cross-stage seam types (Grid produces, Look consumes)
      search.ts                      unchanged
    ipc/
      commands.ts                    EXTEND up front: imageJournal, imageMetadata, reviseEvent,
                                     retractEvent, unretractEvent, redactEvent, imageAbsPath,
                                     revealInFileManager, revealFolder, openWithDefault, rescanRoot
      urls.ts                        unchanged
    components/
      shell/
        Titlebar.svelte              MOVE+EXTEND  accessories obey lights-out
        Indicator.svelte             REWRITE      renders logic/segments.ts output: hairline ·
                                     scope dot (pulse target) · segment texts; hover = scope
                                     popover (Popover); click = summon note. EXEMPT from Tab
                                     (modes must stay visible — flagged decision)
        Cheatsheet.svelte            NEW          Sheet + actions/cheatsheet.ts groups (?/F1)
        ContextMenuHost.svelte       NEW          ONE host: renders shell.contextMenu via
                                     Popover+Menu for all seats; sort ▾ opens the same machinery
        NoteInput.svelte             MOVE         unchanged behavior; exempt from Tab (transient)
        FirstRun.svelte              MOVE         gains EmptyState
        DropConfirm.svelte           NEW          drag-folder → register-root confirm (Sheet)
      rail/
        SourceRail.svelte            NEW   Panel(left) + SourceList + rail-folder menu; resizable,
                                     persisted, \ toggles, push; replaces Rail.svelte (dwell
                                     hot-zone and pin affordance deleted)
        SourceList.svelte            NEW   renders SourceSection[] — folders today; collections/
                                     saved-searches are sibling sections in M3, zero edits
      grid/
        GridSurface.svelte           NEW   header + grid + marquee + Ctrl+wheel size wiring
        GridHeader.svelte            NEW   extracted from App.svelte: folder name, sort ▾ (Menu —
                                     SortMenu.svelte deleted), size slider, tooltips
        Grid.svelte                  MOVE+REWORK  virtualizer over DisplayUnit[]; gridlayout snap;
                                     scroll anchor save/restore
        Thumb.svelte                 MOVE+EXTEND  active ring distinct from selected; T cell-info
                                     levels; chevron slot; single click zone + the one chevron
        StackChevron.svelte          NEW   the expand control (a control, not a badge)
        Marquee.svelte               NEW   rubber-band rendering only (math in logic/marquee.ts)
      look/
        LookSurface.svelte           NEW   stage + bottomEdge region (Filmstrip now; M4 scrubber
                                     is an alternate child) + backdrop menu wiring
        LookStage.svelte             NEW   image + zoom.ts transform; dblclick toggle; drag-pan;
                                     Space-hold-pan; overlay slot reserved for the M2a stroke canvas
        Filmstrip.svelte             NEW   extracted strip (stack-aware), obeys Tab
      inspector/
        Inspector.svelte             NEW   Panel(right) + its own small tab strip (I=Metadata,
                                     J=Journal; M5 adds Partner) — no Tabs primitive (one consumer)
        MetadataTab.svelte           NEW   read-only rows; copyable hash/path (K16 stands)
        JournalTab.svelte            NEW   vertical timeline from day one (M4 = rendering upgrade);
                                     "Nothing yet." EmptyState
        JournalEntry.svelte          NEW   row states + hover actions: Correct (inline revision) ·
                                     Retract (toast+Undo) · Redact…
        RedactionModal.svelte        NEW   the app's ONE modal (R5), frame owned here: focus trap,
                                     default-focus Cancel, §8.4 required copy, confirmhold.ts
      search/
        SearchOverlay.svelte  SearchResultRow.svelte   MOVE only
    debug/DebugPanel.svelte          unchanged

src-tauri/src/
  commands/                          NEW dir (commands.rs deleted — split for parallel ownership)
    mod.rs       shared helpers (S<'a>, emit_pulse, indicator()), submodule decls
    capture.rs   set_scope, indicator_state, add_note, set_rating, report_activity   (moved)
    search.rs    search                                                              (moved)
    library.rs   list_roots, add_root, remove_root, folder_tree, list_folder,
                 ingest_status + NEW rescan_root                                     (moved+1)
    app.rs       settings_get, runtime_status, export_journal, rebuild_index,
                 open_settings_window, quit                                          (moved)
    journal.rs   NEW [Stage C] image_journal, image_metadata, revise_event,
                 retract_event, unretract_event, redact_event → RedactReportDto
                 (wraps store.folded_journal / append / redact)
    os.rs        NEW [Stage A] reveal_in_file_manager, reveal_folder,
                 open_with_default, image_abs_path (D4; no deletion verbs — D3)
  dto.rs         EXTEND: twins of types/dto.ts additions (all up front)
  lib.rs         module decls (FOUNDATIONS); handler registration + tauri-plugin-window-state +
                 tauri-plugin-opener (INTEGRATION)
```

Deliberately NOT built (minimality — each was in a draft and cut): a `Tabs` primitive (one consumer: Inspector, even counting M5 — the tab strip lives inline), a generic `Modal` primitive (R5 guarantees exactly one modal forever; RedactionModal owns its frame), a `Strip` primitive (one consumer: the indicator — segments are pure data from logic/segments.ts), a separate `Toast.svelte` (row markup lives in ToastHost), a `chrome.ts` lights-out module (it is one boolean: every chrome region renders `open && !chromeHidden`, so restore-on-untoggle is automatic).

---

## 2. Primitive API sketches

**Menu.svelte / menu.ts** — props: `model: MenuModel`, `onaction(a: Action)`, `onclose()`; renders inside Popover. MenuModel rows: `{ action; verb; keyHint?; kind: "item"|"radio"|"submenu"|"separator"; checked?; disabled?; children? }`. menu.ts: pure ↑↓→←/Enter/typeahead navigation controller, unit-tested. Consumers: all four context-menu seats, sort ▾, every submenu, M2a tool menus.

**Popover.svelte** — props: `anchor: {x,y} | HTMLElement`, `placement?`, `ondismiss()`, children snippet. Outside-pointer dismiss; Esc routed through escape.ts. Consumers: Menu, indicator scope popover, tooltip body.

**Panel.svelte / panel.ts** — props: `side: "left"|"right"`, `open`, `size` (bindable), `minSize`, `maxSize`, `persistKey`, `label`, children. **Push, never overlay** — participates in the shell flex row, so the grid re-snaps integer columns on resize. panel.ts: clamp/persist math. Consumers: SourceRail, Inspector, M5 partner host.

**Sheet.svelte** — props: `open`, `onclose()`, `label`, children. Quiet centered dismissable sheet (Esc/scrim/[×]); no focus trap. Consumers: Cheatsheet, DropConfirm. New instances are spec changes (guardrail recorded).

**ToastHost.svelte / toast.svelte.ts** — `toast(kind, text, action?: {label, run})` with `kind: "retracted" | "redacted" | "offline-redaction-complete"` — the closed enum IS the three-toast rule. Queue, 5 s auto-dismiss, fixed above the indicator. Fed exclusively by inspector.svelte.ts.

**KeyHint.svelte** — props: `chord: KeyChord`. Platform glyphs (Cmd/Ctrl). The only component allowed to render key names. Consumers: Menu rows, tooltip, Cheatsheet.

**tooltip.ts** — `{@attach tooltip({ actionId })}`: verb + first chord resolved from REGISTRY, rendered in a Popover after hover delay. Consumers: GridHeader controls, rail rows, inspector controls.

**EmptyState.svelte** — props: `line`, optional action snippet. Consumers: FirstRun, empty Grid folder, JournalTab "Nothing yet.".

**SourceList.svelte** (shared component, not primitive) — props: `sections: SourceSection[]`, `focusKey`, `onopen(row)`, `oncontextmenu(row, x, y)`. M3 adds sibling sections with zero edits.

---

## 3. The action system — data flow for actions / keymap / menus / cheatsheet

```ts
// actions/types.ts (contract, frozen by FOUNDATIONS)
interface ActionDef {
  id: Action["kind"];                  // ties registry to the existing Action union
  verb: string;                        // menu text       label?: string;  // cheatsheet long form
  keys: KeyChord[];                    // [] = pointer-only (explicit allowlist)
  scope: "global"|"grid"|"look"|"search"|"inspector";
  seats?: MenuSeat[];                  // "thumb"|"gutter"|"rail-folder"|"look-backdrop"
                                       // reserved, unpopulated in P4.2: "look-toolbar" (M2a)
  group: "contract"|"grid"|"look"|"panels"|"capture"|"search"|"system";
  available: (ctx: ActionContext) => boolean;   // exists in this context (menu visibility)
  enabled?:  (ctx: ActionContext) => boolean;   // runnable now (graying, key gating)
  worksInInput?: boolean;              // exempt from §11 single-letter suppression
  toAction?: (ctx, arg?) => Action;    // parametrized rows (rate N, surround X, sort mode)
  reserved?: true;                     // P/E/V, M, overlay-cycle: dispatch to nothing,
                                       // hidden from menus + cheatsheet until their packet
}
interface ModeDef {                    // "modes are visible" — by construction
  id: "auto-advance" | "pencil" | "mic";        // M2a/M2b ids reserved now
  isOn(ctx): boolean;  segment(ctx): { text; title } | null;
}
```

`ActionContext` is a pure snapshot assembled by `ui.actionContext()` each keydown/menu-open: surface, searchOpen, inputFocused, searchInputFocused, hasSelection, selectionCount, activeHash, activeIsPair, activePairCollapsed, railOpen, railFocused, inspectorOpen (false|"metadata"|"journal"), cheatsheetOpen, contextMenuOpen, chromeHidden, autoAdvance, lookAtFit, debugEnabled, asrReady, plus radio-state fields for menus (sort, thumbStep, surround, filmstrip) and reserved `pencilMode`/`micArmed` (always falsy in P4.2). `KeyContext` (logic/keymap.ts) becomes an alias with the new fields optional-with-defaults, so existing test fixtures compile unchanged.

```
 keydown ──▶ keymap.dispatch (= match.ts over REGISTRY) ──┐
 right-click ──▶ menus.ts(seat, ctx) ─ item.action ───────┼─▶ Action ─▶ ui.perform(Action)
 ?/F1 ──▶ cheatsheet.ts(ctx)        (display only)        │        │ router in state/app.svelte.ts
 tooltip.ts / KeyHint               (display only)        │        ▼
 component-local pointer (dblclick, Ctrl+wheel, marquee) ─┘   slice method → ipc → core echo/pulse
                                                                   ▼
                              modes.ts + logic/segments.ts → Indicator (scope · n/m · A▸ · [seats])
```

One Action type, one perform sink, four renderings of one table. "Context menus mirror every keyboard verb" is true **by construction** (menus query the registry by seat); registry invariant tests (§11) guard the rest. The Space dual-role is the model's showcase: `look-close` is `available` in Look and `enabled: ctx.lookAtFit`; pan-hold is a pointer pipeline fact gated `!lookAtFit` — no special-case branch in any component.

**keymap.ts compatibility:** `dispatch` keeps its exact signature; the Action union is extended (lights-out, go-grid, select-none, cycle-cell-info, stack verbs, zoom-100, look-close, open/close-inspector-tab, journal row verbs, fullscreen, cheatsheet, surround, reveal/copy-path/open-default, edge/page moves). Exactly three existing test expectations change, each citing its amendment in-test: (1) `Tab` → lights-out, `\` → toggle-rail (D5); (2) `Space` in Grid → open-look (§0 symmetry); keyboard selection-toggle moves to `Ctrl+Space` (keeps R6 completeness; not a hot-path per-image verb, so the no-chorded-verbs guardrail is not violated — DECISIONS entry); (3) rail arrow routing gates on `railFocused`, not `railOpen` (the rail is now push-persistent).

---

## 4. State slices

`app.svelte.ts` stays the single `ui` export but becomes a composition root over four slice classes, each in its own file, each contract (fields + method signatures) **frozen by FOUNDATIONS**. Slices never import each other; the root passes what they need. Cross-slice flows live only in the root: `openLook(unit, navSet)` (entry selection → LookEntry order via looknav.ts), `leaveLook()` (scroll-anchor restore, same image active), `goHome()` (G), auto-advance after rate/note-from-Look (logic/advance.ts; multi-select rating never advances or destroys the selection), and scope reporting — `grid.selectionTargets` is already stack-expanded (stacks.expandTargets, JPEG then RAW), so a collapsed pair reports both hashes as one ordered multi-target list; backend untouched (K13).

---

## 5. Rust shell

`commands.rs` (436 lines) splits as listed in §1 so parallel stages own disjoint Rust files. FOUNDATIONS performs the split as pure moves, adds `rescan_root` to library.rs, creates `journal.rs`/`os.rs` as empty modules, and extends dto.rs + ipc/commands.ts + types/dto.ts **contracts-first**. Stages A and C implement bodies + inline `#[cfg(test)]` tests in their own files. INTEGRATION registers the new handlers in lib.rs and adds `tauri-plugin-window-state` (geometry) and `tauri-plugin-opener` (reveal/open-default); "Copy file path" = `image_abs_path` + `navigator.clipboard`. Stack pairing needs no Rust — display-level over GridItem basename+relPath (D1/K13).

---

## 6. Contract mechanics

**Escape order** (escape.ts, one flag per layer, exhaustively tested): 1 redaction modal (Cancel) → 2 context menu (incl. sort ▾) → 3 inline journal correction (text-edit exits first, §0) → 4 note input → 5 cheatsheet → 6 indicator popover → 7 debug panel → 8 inspector → 9 search → 10 Look→Grid (same image active) → 11 clear selection → 12 none (never quits).

**Lights-out**: `Tab` toggles `shell.chromeHidden`; every chrome region in App.svelte (and Filmstrip inside LookSurface, Titlebar accessories) renders `open && !chromeHidden` — open-state survives, restore is automatic, and future chrome obeys by construction because App mounts chrome only through gated regions. Documented exemptions: the indicator (modes must stay visible) and the transient note input. Tab is consumed globally in the main window (a11y trade recorded).

**Space in Look** (looknav.ts, unit-tested): at fit → close; while zoomed → hold-to-pan; a clean tap (down/up, no pointer move) while zoomed → close. Must survive M2a (pencil claims the pointer, Space-pan stays).

---

## 7. [MVP] featureset → structure map (adversarially walked — every item has a named home)

| Featureset item | Home |
|---|---|
| §0 Esc sacred, exits text inputs first | logic/escape.ts 12-layer order + escape.test.ts |
| §0 `G` go home | defs/global.ts → app.svelte.ts goHome() |
| §0 Symmetric open/close | defs/grid.ts open-look (Enter+Space; dblclick in Thumb) ⇄ defs/look.ts look-close (Space at fit, Esc) + looknav.ts tap rule |
| §0 One window, same keys everywhere | one REGISTRY, scope precedence in match.ts |
| §0 Modes visible | actions/modes.ts → logic/segments.ts → Indicator.svelte |
| §0 Tab lights-out, `\` rail | defs/global.ts + shell.chromeHidden + App.svelte gated regions |
| §1 Uniform cells, integer-column snap, recompute on resize/panel | logic/gridlayout.ts + Grid.svelte (Panel resize triggers re-snap) |
| §1 Ctrl+wheel size (synced slider/`-`/`=`) | GridSurface.svelte wheel → same thumb-size action |
| §1 Marquee gutter-only, Ctrl additive | logic/marquee.ts (classifyDrag + hit-test) + grid/Marquee.svelte + selection.ts merge |
| §1 Modifier clicks; Ctrl+A / Ctrl+Shift+A | logic/selection.ts (selectNone new) + defs/grid.ts |
| §1 Active vs selected; write scope unchanged | selection.ts focus≡active (documented) + Thumb active ring; scope via grid.selectionTargets |
| §1 Badges hover-quiet; `T` cycles cell info | logic/cellinfo.ts + Thumb.svelte + defs/grid.ts + prefs |
| §1 Scroll preserved; Home/End/PgUp/PgDn | gridlayout.ts anchors + grid.svelte.ts + selection.ts moves |
| §2 Wheel zoom-to-cursor + anchor-bug fix | logic/zoom.ts (container-relative; letterbox edges unit-tested) + LookStage.svelte |
| §2 Z / Ctrl+0 / Ctrl+1 / dblclick / drag-pan / Space-hold-pan | defs/look.ts + zoom.ts + LookStage pointer pipeline |
| §2 Zoom persists across ←/→; entry = Fit | zoom.ts carryOver({mode,scale,centerFrac}) + look.svelte.ts zoomSession |
| §2 Navigation set = entry selection | looknav.ts navigationSet() + app.svelte.ts openLook |
| §2 Filmstrip `F` default hidden; n-of-m in indicator | look/Filmstrip.svelte + segments.ts n-of-m |
| §3 Left rail: source list, resizable/persisted/`\`/push | rail/SourceRail.svelte (Panel) + SourceList + logic/sources.ts |
| §3 Right inspector, I/J tabs, Esc closes first | inspector/Inspector.svelte (Panel + inline tab strip) + escape layer 8 |
| §3 Metadata tab read-only | MetadataTab.svelte + logic/metadata.ts + journal.rs image_metadata |
| §3 Journal tab: folds/retract/redact/stubs | JournalTab + JournalEntry + logic/journal.ts + journal.rs |
| §3 Redaction modal + required copy; sanctioned toasts | RedactionModal.svelte + confirmhold.ts + toast.svelte.ts kind enum |
| §3 No auto-hide fly-outs | dwell hotzone + pin deleted; Panel has no auto-hide mode |
| §4 Auto-advance `A`, visible, default OFF persisted | logic/advance.ts + modes.ts segment + prefs + root wiring |
| §4 Bare 0–5, 0 clears | defs/global.ts rate (existing semantics kept) |
| §4 Segmented indicator + reserved mic seat | logic/segments.ts ordering reserves the seat |
| §5 Auto-pair, live reversible collapse (per-pair + global), JPEG preview | logic/stacks.ts + StackChevron + Stacks ▸ in gutter menu |
| §5 Collapsed annotate targets both hashes | stacks.expandTargets → grid.selectionTargets → set_scope (one multi-target event) |
| §5 `R` flips member in Look (acts on active in Grid) | defs/look.ts + defs/grid.ts + looknav.ts/stacks.ts |
| §6 Context menus mirror every verb (thumb/gutter/rail/backdrop) | seats on defs + actions/menus.ts + ContextMenuHost + Menu |
| §6 `?`/F1 key map overlay | actions/cheatsheet.ts + shell/Cheatsheet.svelte (Sheet) |
| §6 Empty states; key-hint tooltips | EmptyState.svelte + tooltip.ts |
| §6 Drag folder → register confirm | App.svelte onDragDrop + shell/DropConfirm.svelte (Sheet) |
| §6 Window geometry; F11 | tauri-plugin-window-state + defs/global.ts fullscreen → shell slice |
| §6 D4 reveal / copy path / open default; rail rescan | commands/os.rs + library.rs rescan_root + thumb/rail menu seats |
| §7 Surround luminance + backdrop right-click | theme/tokens.css [data-surround] + theme/surround.ts + Surround ▸ on gutter & look-backdrop seats + prefs |

## 8. Future seats — composition from the same primitives (adversarially walked — no seat needs new chrome)

- **M2a pencil toolbar**: reserved P/E/V registry rows become real; a `pencil` ModeDef lights its indicator segment (`ActionContext.pencilMode` exists); the toolbar = the reserved `"look-toolbar"` MenuSeat rendered by a thin strip over the same MenuModel; LookStage's overlay slot mounts the stroke canvas; zoom.ts is pointer-agnostic so wheel-zoom stays live; pencil-red token already in tokens.css; Space-pan already exists.
- **M2b mic**: `mic` ModeDef fills the segment seat logic/segments.ts already orders; `M` is a reserved row gated `asrReady`; recording state lives in the segment — the toast kind-enum makes a recording toast impossible.
- **M3 source rail + query residue**: collections/saved-searches register as SourceSection providers in logic/sources.ts (SourceList renders siblings, zero rail edits); active-query residue = the reserved segment id + one one-key-clear registry row; drag-selection-to-rail is the `"item-drag"` branch marquee.classifyDrag already distinguishes.
- **M4 scrubber + timeline journal**: the scrubber is an alternate child of LookSurface's bottomEdge region (both obey Tab); the per-image timeline is a rendering upgrade inside JournalTab — logic/journal.ts and the surface unchanged.
- **M5 partner panel**: one more tab in Inspector's inline tab strip on the same Panel — the right edge stays the per-image-truth seat; zero new layout machinery.
- **Full themes (post-M1)**: a `[data-theme]` token block; no component changes.

## 9. Stage partition (summary — exact lists in the ownership map)

FOUNDATIONS (sequential) → Stage A (Grid+stacks) ∥ Stage B (Look) ∥ Stage C (Inspector+journal) → INTEGRATION (sequential). FOUNDATIONS owns every shared file and scaffolds every stage-owned file as a compiling stub (slice contracts, empty def arrays, placeholder components mounted by the final App.svelte), so the gate is green at handoff and parallel stages edit strictly disjoint files — none of them ever touches App.svelte, app.svelte.ts, registry.ts, menus.ts, ipc/commands.ts, types/dto.ts, prefs.ts, lib.rs, dto.rs, or commands/mod.rs.

## 10. Testing strategy per layer

1. **Pure logic (vitest — the bulk, per the featureset's acceptance list)**: zoom (anchor invariance both axes, letterboxed edges, clamp, carryOver across mismatched dims), gridlayout (snap fills width exactly, anchors), marquee (rect→indices, additive, drag classification), stacks (pairing, live reversibility, target order JPEG→RAW, flip), selection (active model, select-none, edge/page), looknav (entry-set cycling, Space tap-vs-hold, R), journal display states, metadata, advance, sources, segments, surround, confirmhold, escape (full 12-layer), scope (collapsed-pair expansion + CAPTURE target-order conformance).
2. **Registry invariants (tests/registry.test.ts)**: ids unique; property sweep over generated ActionContexts asserting no two defs both `available+enabled` for the same chord; every def has ≥1 key or seat or is on the pointer-only allowlist; cheatsheet rows ≡ registry minus reserved; every seatable verb appears in ≥1 seat or on a named exemption list. Parity is structural; these tests guard the residue.
3. **Keymap**: existing keymap.test.ts kept (3 expectations amended with in-test citations); new rows tested in per-stage grid-keys / look-keys / inspector-keys files — never by editing the existing blocks.
4. **Primitive controllers** (menu.ts, panel.ts, toast.svelte.ts): pure-unit; behavior is pushed into controllers by construction so Svelte wrappers stay thin.
5. **Store slices**: existing ui-store pattern (mocked `@tauri-apps/api/core`) split per slice; integration app-flows test (G/Tab round-trips, openLook nav-set, scroll-anchor restore, auto-advance, scope echo with stacks, toast queue, redact flow).
6. **Render smoke (search-render.test.ts pattern, only where DOM is load-bearing)**: Menu keyboard nav + graying, RedactionModal focus trap/default-Cancel, JournalTab display states, Indicator segments incl. modes, Cheatsheet groups.
7. **Rust**: inline `#[cfg(test)]` per command module against temp EventStore/Library (state.rs pattern): journal fold/revise/retract/unretract/redact round-trips, redact report with offline volumes, metadata mapping, os path resolution. Standing gates unchanged (cargo fmt/clippy/test; npm check/build/vitest).
8. **Eyes-only — named in DOGFOOD-M1.md §visual (integration)**: 60 fps with push panels resizing over 20k items, marquee feel, zoom-at-cursor on trackpad, surround legibility on real images, lights-out instantaneity, toast placement, F11/geometry on Linux/Wayland, context-menu completeness by hand on all four seats.

## 11. Decisions to record in spec/DECISIONS.md (integration stage)

1. Space opens/closes Look (featureset §0 supersedes UI.md §3.4 Space-toggle); keyboard selection-toggle moves to **Ctrl+Space**.
2. Tab is consumed globally for lights-out (D5); webview Tab focus-traversal forfeited in the main window (a11y note; arrows/Enter remain).
3. Rail and inspector are **push** panels with no auto-hide/dwell/pin — supersedes UI.md §3.7 "overlay, not push" and §8.1 slide-over; inspector width persists, openness does not.
4. Collapsed-stack event target order: display member (JPEG) first, then RAW (CAPTURE event_targets.position).
5. Indicator and transient note input are exempt from lights-out (modes must stay visible) — founder sign-off requested.
6. Surround surfaces only via backdrop/gutter right-click + persisted pref (Settings §2.4 enumeration stays closed).
7. Sheet instances are enumerated (cheatsheet, drop-confirm); new instances are spec changes.

---

# Appendix A — File-ownership map (stage contracts)

```
# File-ownership map — P4.2 (paths relative to apps/desktop/ unless src-tauri/)
# Rule: FOUNDATIONS (sequential, first) may create/move any file once, scaffolding stage-owned
# files as compiling stubs. From parallel kickoff, the listed owner is exclusive. A ∥ B ∥ C are
# strictly file-disjoint; App.svelte and state/app.svelte.ts are touched ONLY by F and I.

## STAGE F — FOUNDATIONS (sequential; owns all shared files)
Frontend:
- src/app.css (slim) · src/lib/theme/tokens.css · src/lib/theme/surround.ts
- src/lib/primitives/{Menu.svelte, menu.ts, Popover.svelte, Panel.svelte, panel.ts, Sheet.svelte, ToastHost.svelte, toast.svelte.ts, KeyHint.svelte, tooltip.ts, EmptyState.svelte}
- src/lib/actions/{types.ts, match.ts, registry.ts, menus.ts, cheatsheet.ts, modes.ts}
- src/lib/actions/defs/{global.ts, search.ts, rail.ts}  (+ stubs of defs/{grid,look,inspector}.ts handed to A/B/C)
- src/lib/logic/{keymap.ts (interpreter, signature preserved), escape.ts, selection.ts, scope.ts, advance.ts, segments.ts, sources.ts}
- src/lib/types/{dto.ts (all new DTOs), display.ts} · src/lib/ipc/commands.ts (all new wrappers)
- src/lib/state/{app.svelte.ts (contracts + cross-slice stubs), shell.svelte.ts (complete), prefs.ts (all new keys)}  (+ contract skeletons of grid/look/inspector.svelte.ts handed to A/B/C)
- src/lib/components/shell/{Titlebar.svelte (move), Indicator.svelte (rewrite), Cheatsheet.svelte, ContextMenuHost.svelte, NoteInput.svelte (move), FirstRun.svelte (move)}
- src/lib/components/rail/{SourceRail.svelte, SourceList.svelte}
- src/lib/components/search/{SearchOverlay.svelte, SearchResultRow.svelte} (move only)
- src/App.svelte (rewrite: gated chrome regions, menu/toast/cheatsheet hosts, hotzone removal; mounts stage stubs)
- component scaffolds handed off: src/lib/components/grid/* to A, look/* to B, inspector/* to C (legacy Grid/Thumb/Look content moved in as the starting point)
- deletions recorded here: src/lib/components/{Rail,SortMenu,Titlebar,Grid,Thumb,Look,Indicator,NoteInput,FirstRun,SearchOverlay,SearchResultRow}.svelte (old flat paths — parallel stages never touch them)
Tests:
- tests/{keymap.test.ts (3 amended rows), escape.test.ts, selection.test.ts, scope.test.ts, registry.test.ts, advance.test.ts, segments.test.ts, sources.test.ts, surround.test.ts, menu.test.ts, panel.test.ts, toast.test.ts, shell-slice.test.ts, ui-store.test.ts (reorganized green)}
Rust:
- delete src-tauri/src/commands.rs; create src-tauri/src/commands/{mod.rs, capture.rs, search.rs, library.rs (+rescan_root), app.rs, journal.rs (empty module), os.rs (empty module)}
- src-tauri/src/dto.rs (all new DTOs) · src-tauri/src/lib.rs (module decls + rescan_root registration)

## STAGE A — GRID + STACKS (parallel)
- src/lib/components/grid/{GridSurface.svelte, GridHeader.svelte, Grid.svelte, Thumb.svelte, StackChevron.svelte, Marquee.svelte}
- src/lib/logic/{gridlayout.ts, marquee.ts, stacks.ts, cellinfo.ts}
- src/lib/actions/defs/grid.ts
- src/lib/state/grid.svelte.ts (implement within frozen contract)
- src-tauri/src/commands/os.rs (reveal_in_file_manager, reveal_folder, open_with_default, image_abs_path + inline tests)
- tests/{gridlayout.test.ts, marquee.test.ts, stacks.test.ts, cellinfo.test.ts, grid-keys.test.ts, grid-slice.test.ts}

## STAGE B — LOOK (parallel)
- src/lib/components/look/{LookSurface.svelte, LookStage.svelte, Filmstrip.svelte}
- src/lib/logic/{zoom.ts, looknav.ts}
- src/lib/actions/defs/look.ts
- src/lib/state/look.svelte.ts (implement within frozen contract)
- tests/{zoom.test.ts, looknav.test.ts, look-keys.test.ts, look-slice.test.ts}

## STAGE C — INSPECTOR + JOURNAL (parallel)
- src/lib/components/inspector/{Inspector.svelte, MetadataTab.svelte, JournalTab.svelte, JournalEntry.svelte, RedactionModal.svelte}
- src/lib/logic/{journal.ts, metadata.ts, confirmhold.ts}
- src/lib/actions/defs/inspector.ts
- src/lib/state/inspector.svelte.ts (implement within frozen contract)
- src-tauri/src/commands/journal.rs (image_journal, image_metadata, revise_event, retract_event, unretract_event, redact_event + inline tests)
- tests/{journal.test.ts, metadata.test.ts, confirmhold.test.ts, inspector-keys.test.ts, inspector-slice.test.ts, inspector-render.test.ts}

## STAGE I — INTEGRATION (sequential, last)
- src/App.svelte (finish: drop handler, final wiring, edge cases)
- src/lib/state/app.svelte.ts (finish cross-slice flows: openLook nav-set, leaveLook scroll restore, goHome, auto-advance, scope with stacks)
- src/lib/components/shell/DropConfirm.svelte
- src-tauri/src/lib.rs (register journal/os handlers; window-state + opener plugins) · src-tauri/Cargo.toml · src-tauri/capabilities/default.json · src-tauri/tauri.conf.json (as needed)
- tests/{app-flows.test.ts, contract.test.ts (Esc 12-layer end-to-end, G from everywhere, Tab hides all regions, menus-mirror-keyboard + cheatsheet completeness over the final registry)}
- docs/DOGFOOD-M1.md (§visual additions) · spec/DECISIONS.md (the seven recorded decisions)

## Disjointness check (A ∥ B ∥ C)
components: grid/* vs look/* vs inspector/* — disjoint. logic: {gridlayout,marquee,stacks,cellinfo} vs {zoom,looknav} vs {journal,metadata,confirmhold} — disjoint. actions: defs/grid.ts vs defs/look.ts vs defs/inspector.ts — disjoint (registry.ts/menus.ts frozen in F). state: grid vs look vs inspector slices — disjoint (app.svelte.ts/shell.svelte.ts F/I-only). Rust: os.rs vs ∅ vs journal.rs — disjoint (lib.rs/dto.rs/mod.rs F/I-only). Tests: all file names distinct. Cross-stage data flows only through F-frozen seams: types/display.ts (DisplayUnit/LookEntry), slice contracts, ActionContext, dto.ts.
```

# Appendix B — Open risks (tracked through implementation)

- Contract freeze is the linchpin: A/B/C compile against slice/DTO/ActionContext/registry contracts written in FOUNDATIONS; any mid-flight change (DisplayUnit shape, look-slice fields the indicator reads, a new ActionContext field) forces serialization. Mitigation: FOUNDATIONS lands all existing tests green against the stub contracts before kickoff, and only INTEGRATION may amend app.svelte.ts; two failed verify cycles = stop and replan per BUILD-LOOP.
- photoproof-core verification gap: store.append/folded_journal/redact are confirmed present, but no un-retract API was found in store/mod.rs, and revision/retraction draft ergonomics are unverified. If Undo-on-retract needs core work, Stage C silently grows a core-crate dependency and breaks the file partition. Mitigation: verify and pre-land any core gap during FOUNDATIONS, before Stage C starts.
- Tab = lights-out consumes the webview's focus-traversal key globally (preventDefault in the main window) — an accessibility trade against UI.md §12's keyboard-completeness baseline; must be scoped to the main window (not Settings) and recorded in DECISIONS.md. The indicator/note-input exemption from lights-out also needs explicit founder sign-off (the featureset enumerates rail/inspector/filmstrip/titlebar but is silent on the indicator).
- Space's triple role in Look (close at fit / hold-to-pan zoomed / clean-tap-close zoomed) is the most fragile interaction in the packet; looknav.ts needs exhaustive tap-vs-hold unit tests plus a DOGFOOD feel check, and it must not regress the M2a prerequisite (Space-pan must survive the pencil claiming the pointer).
- Push panels contradict UI.md §3.7's 'summoning never reflows the grid' acceptance line and force integer-column re-snap + virtualizer re-layout during panel-edge drag; 60 fps on 20k items while resizing is unproven. Mitigation: resize math is pure derived work in gridlayout.ts, throttled resize observation, named DOGFOOD perf item; the spec amendment goes to DECISIONS.md.
- Collapsed-stack multi-target events (one event, JPEG-then-RAW order) change what event_targets.position consumers see (sidecars, journal attribution, search provenance), and the indicator's '● N' diverges from visible cell count (1 cell, 2 targets). Needs an explicit CAPTURE-conformance test in scope.test.ts plus a dogfood copy check, or the invisible-sidecar bug this feature exists to kill reappears in a new form.
- Registry chord-collision space grows nonlinearly with ActionContext fields; the invariant property test must sweep generated contexts (not hand-picked fixtures) or a collision ships as a silently dead key. Same for gating parity: available/enabled predicates live only in defs (pure, tested) — any component-level enablement check is a review-time smell.
- FOUNDATIONS is large (action system + store split + primitives + component moves + Rust split + all IPC/DTO contracts) and sits alone on the critical path. Mitigation: a mid-stage checkpoint (action system + keymap green) lets Stage B — the smallest stage, no Rust — start early; Stage A is the largest parallel packet (marquee + stacks + active/selected + cellinfo + os.rs) and should pre-agree an internal landing order (stacks → selection/marquee → cellinfo/menus) so partial merges stay green.
- tauri-plugin-window-state and F11 interact poorly with custom titlebars on some Linux/Wayland compositors (restore drift, decoration glitches); geometry persistence may need a manual fallback in commands/app.rs — contained in INTEGRATION but can eat its schedule. Clipboard for 'Copy file path' (navigator.clipboard in webview) needs the same platform smoke check.
- Zoom persistence across ←/→ must reapply {mode, scale, centerFrac} to images of different dimensions inside the <150 ms swap budget; naive transform reuse on mismatched aspect ratios reintroduces the anchor-drift bug class. zoom.test.ts must cover dimension-change carryOver explicitly, not just single-image anchoring.
