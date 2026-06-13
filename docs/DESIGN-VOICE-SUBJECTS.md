# DESIGN: Voice dictation to a collection/topic note log (scope subjects)

Status: accepted (founder, June 13 2026 - "Yes, voice can note a collection/topic").
Implementation contract.

## The problem

Today the capture scope is a list of image content-hashes
(`ScopeSnapshot.targets: Vec<ContentHash>`), and a voice final mints an
image-targeted `Remark` event (`event_targets`, image-hash only). Collections
and topics each have their OWN append-only note log (`collection_notes`,
`topic_notes`) - typed text only. The founder wants dictation to be able to land
in those subject note logs, not just on images.

## The interaction rule (the crux)

Dictation routes by what is in focus, reusing the existing neutral-scope idea:

- **An image is focused/selected** (grid focus, Look current, or a selected
  visualizer node -> `activeHash != null`): dictation mints an IMAGE note, exactly
  as today. A focused image ALWAYS wins.
- **A collection or topic detail is open AND no image is focused** (`activeHash`
  is null, i.e. the scope would otherwise be the neutral session scope): dictation
  appends to that subject's note log (`collection_notes` / `topic_notes`).
- **Nothing focused and no subject open**: unchanged - a zero-target session note.

This is discoverable because the subject's note composer panel
(`CollectionNotes` / `TopicNotes`) is already visible in the rail when the subject
is open, and the scope/capture indicator names the subject (see "Indicator"
below), so the user sees where their words will land before speaking.

Why focused-image-wins: dictating about a specific member image while inside a
collection must keep working. Deselecting (Esc to clear grid focus, or simply not
having focused a member) is the natural "I am talking about the collection itself"
gesture.

## The model

### Scope subject
Extend the capture scope to carry an optional non-image subject:

```rust
// capture/scope.rs
pub enum ScopeSubject {
    Collection(String),   // collection id (ULID)
    Topic(String),        // topic id (ULID)
}
// ScopeSnapshot gains:
pub subject: Option<ScopeSubject>,
```

Invariant: a snapshot has EITHER image `targets` (subject = None) OR a `subject`
with empty `targets`. The frontend never sends both; if both somehow arrive,
image targets win (subject ignored) so an image note is never silently lost.

### Frontend: reportScope derives the subject
`scopeTargets`/`reportScope` (app.svelte.ts) gains a subject derivation:

- If `activeHash != null` -> image targets (current behavior), subject = None.
- Else if `viewMode`/rail context has a collection open (`collectionId != null`)
  -> subject = `Collection(collectionId)`, targets = [].
- Else if a topic detail is open (`topicDetailId != null`)
  -> subject = `Topic(topicDetailId)`, targets = [].
- Else -> neutral (targets [], subject None) = session note.

The `set_scope` IPC command carries the optional subject alongside targets. The
scope ring stores it on each snapshot (so a subject, like image targets, is bound
at speech onset and frozen for the utterance - consistent with the existing
onset-binding model, including the spanning-swap union which only applies to image
targets).

### Backend: on_final routes the mint
`engine.on_final` branches on the held snapshot:

- `subject == None` (or non-empty image targets): existing image `Remark` event
  path - UNCHANGED.
- `subject == Some(Collection(id))`: append the transcript to `collection_notes`
  for `id` (the same store method the typed composer uses).
- `subject == Some(Topic(id))`: append to `topic_notes` for `id`.

The engine reaches the note-append methods through its store handle (the same DB
the EventStore writes). The voice note text is verbatim (K14: user-authored
speech; the machine routes, never composes). Confidence/duration metadata that the
image `Remark` carries has no column in the note tables - drop it for v1 (the note
tables are plain `id/subject_id/ts/text`); revisit if a voice-vs-typed
distinction is wanted later.

### Indicator
When the bound scope is a subject, the capture/scope indicator (shell scope pill)
names it ("noting: <collection name>" / "noting topic: <phrase>") so the user has
feedback that dictation targets the subject, not an image. No em-dashes in the
copy.

## Spanning a swap
The image-swap union (a dictation that crosses image nav lands on all viewed
images) applies to IMAGE targets only. If the user starts dictating to a subject
(no image focused) and then focuses an image mid-utterance, the utterance stays
bound to the subject it started on (onset binding); the newly focused image does
not retro-capture. Switching subjects mid-utterance is an edge case - keep the
onset-bound subject (do not union subjects). Document this; it is the least
surprising.

## Tests
- Backend: a final with a `Collection(id)` subject appends to `collection_notes`
  (not an event); same for `Topic(id)` -> `topic_notes`; a final with image
  targets still mints an image event (regression); a subject snapshot with
  empty targets does NOT mint a zero-target session event.
- Frontend: `reportScope` sends subject = collection/topic when a subject is open
  and no image focused; sends image targets (subject None) when an image is
  focused; neutral otherwise. The indicator names the subject.

## Non-goals (v1)
- No voice metadata columns on the note tables.
- No subject-union across a mid-utterance subject switch.
- event_targets stays image-hash only (subjects use their own note tables).
