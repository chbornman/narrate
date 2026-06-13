/**
 * Topic-notes slice (founder decision: topics get their own append-only note
 * log, mirroring CollectionNotesSlice). Owns the loaded notes for ONE viewed
 * topic plus the compose flow, with the same stale-response guard and the same
 * "draft survives a backend failure" contract.
 *
 * It is deliberately SELF-CONTAINED rather than a field on the app store: the
 * rail's Topics view drives it off `ui.topicDetailId` through a local effect,
 * so the contended app store stays untouched. WHY keyed by topic ID (not the
 * phrase the grid scopes on): two saved topics may share a phrase pinned to
 * different spaces, and the note log is per-topic-record, so it hangs off the
 * id exactly as the backend `topic_notes` table's FK does.
 */
import * as ipc from "../ipc/commands";
import type { TopicNoteDto } from "../types/dto";

export class TopicNotesSlice {
  /** The topic whose notes are loaded (null = nothing viewed). */
  id = $state<string | null>(null);

  notes = $state<TopicNoteDto[]>([]);

  /** The compose input holds focus — kept true to the DOM by the
   * component's focus/blur handlers (the collection-notes precedent). */
  composerFocused = $state(false);

  /** Stale-response guard: only the latest load may land (a slow notes
   * fetch must never overwrite a topic the view has since left). */
  #loadSeq = 0;

  /** Load the topic's notes via ipc (commands/topics.rs). null clears the
   * panel — the view left a topic detail (no topic open in the rail). */
  async load(id: string | null) {
    this.id = id;
    const seq = ++this.#loadSeq;
    if (id === null) {
      this.notes = [];
      return;
    }
    try {
      const notes = await ipc.topicNotes(id);
      if (seq !== this.#loadSeq) return; // a newer load superseded this one
      this.notes = notes;
    } catch {
      // Backend unavailable (tests/dev): an honest empty panel.
      if (seq !== this.#loadSeq) return;
      this.notes = [];
    }
  }

  /** Append a note to the viewed topic. Empty/whitespace commits nothing
   * (resolves false so the draft is kept). On success the freshly appended
   * note lands optimistically — append-only, so there is no fold to re-derive,
   * and the id order matches the server's. Resolves whether a note was
   * committed (the composer clears its draft iff so). */
  async compose(text: string): Promise<boolean> {
    const id = this.id;
    if (id === null || text.trim() === "") return false;
    try {
      const note = await ipc.addTopicNote(id, text);
      // Guard the optimistic append against a view that moved on mid-await.
      if (this.id === id) this.notes = [...this.notes, note];
      return true;
    } catch {
      // Backend unavailable: the draft survives (the collection-notes contract).
      return false;
    }
  }
}
