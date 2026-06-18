/**
 * Centralized UI-only tuning constants — the frontend half of the tuning
 * registry (see `docs/DESIGN-TUNING-CONFIG.md`, rendered at `docs/tuning.html`).
 *
 * ONE home for the search-bar knobs that used to sit inline in the components
 * that consume them. These are UI-ONLY for v1: pure client behavior with no
 * backend round-trip. Values the BACKEND owns (the fusion weights, RRF k, beta,
 * preview edges) live in the Rust `tuning` config / `tuning.toml`, NOT here —
 * duplicating them would fork the single source.
 *
 * WHY these stay client-side: both gate the as-you-type keystroke path, which
 * must stay inside the <100 ms results budget (UI §5.1) without a server hop on
 * every keypress. They are deliberately NOT user-editable in v1 (a sliders/panel
 * surface is a later pass); centralizing them is purely "give them one home" so
 * the next person tuning search-bar feel finds every knob in one file.
 */

/**
 * As-you-type keystroke debounce, ms. UI §5.1 allows a debounce of at most
 * 50 ms inside the <100 ms results budget; this value sits AT that spec ceiling
 * (high enough to coalesce a fast typist's keystrokes into one lexical query,
 * low enough that the grid still re-scopes live). The backend's `interrupt()`
 * cancels any in-flight query a newer keystroke supersedes.
 */
export const SEARCH_DEBOUNCE_MS = 50;

/**
 * Minimum free-text query length before a search runs. A single character
 * matches nearly everything and burns the <100 ms budget (UI §5.1) on a result
 * set nobody asked for. Shorter queries with no chips clear the results instead.
 */
export const MIN_QUERY_CHARS = 2;

/**
 * Diversify (duplication-tolerance) slider debounce, ms
 * (DESIGN-DEDUP-AND-SIMILARITY.md). The tolerance slider drives a backend
 * `diversify_scope` pass (a brute-force CLIP k-NN precompute + greedy
 * facility-location) that is FAR heavier than a keystroke search and runs on a
 * blocking thread, so the drag must coalesce hard: 250 ms waits out a continuous
 * slider drag and fires ONE pass when the user settles, rather than one pass per
 * intermediate value. Deliberately well above SEARCH_DEBOUNCE_MS — this is not on
 * a sub-100 ms budget; it is an explicit, opt-in review action where a beat of
 * latency for a much cheaper IPC load is the right trade.
 */
export const DIVERSIFY_DEBOUNCE_MS = 250;
