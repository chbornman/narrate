/**
 * Cross-window preference sync (STATE-INTEGRITY-AUDIT.md: "multi-webview state
 * needs an explicit broadcast"). The app runs TWO webviews (main + Settings),
 * each with its own JS context and localStorage, so a preference set in one does
 * NOT reach the other and does NOT share storage. Any store whose value the user
 * can change in Settings but must take effect in the main window (theme,
 * surround, ...) broadcasts over a Tauri event and every window applies it.
 *
 * Kept tiny + dependency-light so each store wires it in three lines: broadcast
 * on change, subscribe in init(), dispose the returned unsubscribe in dispose().
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";

/** True only inside the Tauri runtime. Outside it (vitest/jsdom, SSR) the
 * emit/listen are no-ops, so stores stay usable in unit tests without mocking
 * the event API and a non-Tauri host never throws. */
export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Fire-and-forget broadcast of a preference change to every window (including
 * this one — the sender's own listener should no-op when the value already
 * matches, which both breaks the echo loop and avoids a redundant repaint). */
export function broadcast(event: string, payload: unknown): void {
  if (inTauri()) void emit(event, payload);
}

/**
 * Listen for a cross-window preference change. Returns an unsubscribe that tears
 * the listener down even if called before the async `listen` resolves (it chains
 * the unlisten onto the promise). A no-op outside Tauri.
 */
export function subscribe<T>(
  event: string,
  handler: (payload: T) => void,
): () => void {
  if (!inTauri()) return () => {};
  const pending: Promise<UnlistenFn> = listen<T>(event, (e) =>
    handler(e.payload),
  );
  return () => {
    void pending.then((un) => un());
  };
}
