/**
 * Search-scope rows — RETIRED by M3 search-as-scope (Phase 1).
 *
 * The old overlay had its own result cursor and selection, so it needed
 * search-scope keymap rows (result nav, open-result, remove-last-chip). The
 * query is a GRID SCOPE now: results are ordinary grid cells, driven by the
 * grid's own focus-move + Enter (open-look). The always-visible header bar
 * is a focused text input that handles its own keys locally (Enter commits
 * the semantic lane; Backspace on an empty input drops the last chip via a
 * direct `remove-last-chip` dispatch) — no search-scope keymap layer.
 *
 * Kept as an empty export so the registry aggregator (registry.ts, frozen)
 * still imports it unchanged; the `search` scope/group survive in the type
 * unions (frozen) for the station's search seat and the cheatsheet group.
 */
import type { ActionDef } from "../types";

export const SEARCH_DEFS: ActionDef[] = [];
