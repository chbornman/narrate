/**
 * Inspector slice — implemented by STAGE C within the FOUNDATIONS-frozen
 * contract (fields + method signatures unchanged; flow methods added).
 * Owns the right panel's open/tab state, the loaded journal entries +
 * metadata for the ACTIVE image, the inline-correction and retract/redact
 * flow state, and is the ONLY feeder of the sanctioned-toast queue
 * (primitives/toast.svelte.ts):
 * retract → "Retracted" + Undo (Undo RE-STATES — a new remark carrying the
 * folded text; retraction-of-retraction is forbidden, DECISIONS E4);
 * redact → the report-driven copy incl. offline-pending volumes.
 */
import * as ipc from "../ipc/commands";
import {
  offlineRedactionToastText,
  redactionToastText,
  RETRACTED_TOAST_TEXT,
} from "../logic/journal";
import { toasts } from "../primitives/toast.svelte";
import type { ImageMetadataDto, JournalEntryDto } from "../types/dto";

export class InspectorSlice {
  /** false = closed; otherwise the open tab (I = metadata, J = journal). */
  open = $state<false | "metadata" | "journal">(false);

  /** Image whose truth is shown (the active image; root keeps it fresh). */
  hash = $state<string | null>(null);

  entries = $state<JournalEntryDto[]>([]);
  metadata = $state<ImageMetadataDto | null>(null);

  /** Per-session "show retracted" toggle (UI §8.2). */
  showRetracted = $state(false);

  /** Inline correction in progress (escape layer 3 closes it first). */
  editingEventId = $state<string | null>(null);

  /** Redaction modal target (escape layer 1 cancels it first). */
  redactTargetId = $state<string | null>(null);

  /** Stale-response guard: only the latest load may land. */
  #loadSeq = 0;

  openTab(tab: "metadata" | "journal") {
    this.open = tab;
  }

  close() {
    this.open = false;
    this.editingEventId = null;
    this.redactTargetId = null;
  }

  /** Fetch journal + metadata for `hash` via ipc (commands/journal.rs). */
  async load(hash: string | null) {
    this.hash = hash;
    const seq = ++this.#loadSeq;
    if (hash === null) {
      this.entries = [];
      this.metadata = null;
      return;
    }
    try {
      const [entries, metadata] = await Promise.all([
        ipc.imageJournal(hash),
        ipc.imageMetadata(hash),
      ]);
      if (seq !== this.#loadSeq) return; // a newer load superseded this one
      this.entries = entries;
      this.metadata = metadata;
    } catch {
      // Backend unavailable (tests/dev): an honest empty panel.
      if (seq !== this.#loadSeq) return;
      this.entries = [];
      this.metadata = null;
    }
  }

  /** Re-fold after any journal mutation (revise/retract/redact/restate). */
  async #reload() {
    await this.load(this.hash);
  }

  // ---------------------------------------------------------------------------
  // journal flows (UI §8.3 — the only place capture is edited)
  // ---------------------------------------------------------------------------

  /** Correct: commit the inline revision (EVENTS §3.4; the fold shows the
   * corrected text, the original stays behind the "edited" affordance).
   * Empty text commits nothing — the edit simply closes. */
  async commitCorrection(eventId: string, text: string) {
    this.editingEventId = null;
    if (text.trim() === "") return;
    try {
      const committed = await ipc.reviseEvent(eventId, text);
      if (committed) await this.#reload();
    } catch {
      /* backend unavailable: the row keeps its current fold */
    }
  }

  /** Retract: immediate tombstone, then sanctioned toast 1 with Undo.
   * Undo = RE-STATE (coordinator ruling / DECISIONS E4): the backend
   * appends a NEW remark/rating carrying the folded content — never an
   * un-retraction. */
  async retract(eventId: string) {
    try {
      const committed = await ipc.retractEvent(eventId);
      if (!committed) return;
      await this.#reload();
      toasts.toast("retracted", RETRACTED_TOAST_TEXT, {
        label: "Undo",
        run: () => void this.restate(eventId),
      });
    } catch {
      /* backend unavailable: nothing retracted, nothing toasts */
    }
  }

  /** The retract-toast Undo (kept callable for tests): re-state. */
  async restate(eventId: string) {
    try {
      const committed = await ipc.unretractEvent(eventId);
      if (committed) await this.#reload();
    } catch {
      /* backend unavailable */
    }
  }

  // ---------------------------------------------------------------------------
  // redaction (the app's ONE modal, R5/§8.4)
  // ---------------------------------------------------------------------------

  /** Redact…: open the modal on this event (escape layer 1 cancels). */
  beginRedact(eventId: string) {
    this.redactTargetId = eventId;
  }

  cancelRedact() {
    this.redactTargetId = null;
  }

  /** The hold-to-confirm commit: scrub everywhere, then sanctioned toast 2
   * with the report-driven copy ("— N offline sidecars pending"). */
  async confirmRedact() {
    const eventId = this.redactTargetId;
    if (eventId === null) return;
    this.redactTargetId = null;
    try {
      const report = await ipc.redactEvent(eventId);
      await this.#reload();
      toasts.toast("redacted", redactionToastText(report));
    } catch {
      /* backend unavailable: nothing scrubbed, nothing toasts */
    }
  }

  /** Sanctioned toast 3: a queued offline-volume redaction completed on
   * mount. The backend event arrives via INTEGRATION wiring; the slice
   * stays the queue's only feeder. */
  offlineRedactionComplete(volumeLabel: string) {
    toasts.toast("offline-redaction-complete", offlineRedactionToastText(volumeLabel));
  }
}
