/**
 * Collection-notes slice (B71 — the composer UI slice over P7.3 commands).
 * Owns the loaded append-only notes for ONE viewed collection plus the
 * compose flow, mirroring InspectorSlice's load/compose shape (the same
 * stale-response guard, the same "draft survives a backend failure"
 * contract).
 *
 * It is deliberately SELF-CONTAINED rather than a field on the app store:
 * the rail's collection view drives it off `ui.collectionId` through a
 * local effect, so the contended app store stays untouched. A collection
 * note is about the GROUPING's intent — never a single image — so this is
 * a sibling of the journal, not a branch of it.
 */
import * as ipc from "../ipc/commands";
import type { CollectionNoteDto } from "../types/dto";

export class CollectionNotesSlice {
  /** The collection whose notes are loaded (null = nothing viewed). */
  id = $state<string | null>(null);

  notes = $state<CollectionNoteDto[]>([]);

  /** The compose input holds focus — kept true to the DOM by the
   * component's focus/blur handlers (the journal-composer precedent). */
  composerFocused = $state(false);

  /** Stale-response guard: only the latest load may land (a slow notes
   * fetch must never overwrite a collection the view has since left). */
  #loadSeq = 0;

  /** Load the collection's notes via ipc (commands/collections.rs). null
   * clears the panel — the view left collections entirely. */
  async load(id: string | null) {
    this.id = id;
    const seq = ++this.#loadSeq;
    if (id === null) {
      this.notes = [];
      return;
    }
    try {
      const notes = await ipc.collectionNotes(id);
      if (seq !== this.#loadSeq) return; // a newer load superseded this one
      this.notes = notes;
    } catch {
      // Backend unavailable (tests/dev): an honest empty panel.
      if (seq !== this.#loadSeq) return;
      this.notes = [];
    }
  }

  /** Append a note to the viewed collection. Empty/whitespace commits
   * nothing (resolves false so the draft is kept). On success the freshly
   * appended note lands optimistically — append-only, so there is no fold
   * to re-derive, and the snapshot order matches the server's. Resolves
   * whether a note was committed (the composer clears its draft iff so). */
  async compose(text: string): Promise<boolean> {
    const id = this.id;
    if (id === null || text.trim() === "") return false;
    try {
      const note = await ipc.addCollectionNote(id, text);
      // Guard the optimistic append against a view that moved on mid-await.
      if (this.id === id) this.notes = [...this.notes, note];
      return true;
    } catch {
      // Backend unavailable: the draft survives (the journal's contract).
      return false;
    }
  }
}
