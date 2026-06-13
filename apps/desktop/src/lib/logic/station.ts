/**
 * The What's-Happening Station model (founder-confirmed, June 2026: the
 * capture indicator grown into the app-wide status organ). ONE pure
 * function over state that already exists — ingest, the RUNTIME snapshot,
 * mic state, the streaming utterance — to `{ pulsing, seats, activities }`.
 * Station.svelte renders this verbatim:
 *
 *   · seats — the COLLAPSED icon row. Every seat is a clickable verb that
 *     dispatches its registry row via resolveAction (the one-action-table
 *     rule). Founder ruling on the status-vs-launcher tension: ICONS are
 *     the click targets; the expanded body is READ-ONLY context — a
 *     hover-to-check-status must never misfire into a verb.
 *   · activities — typed read-only rows the hover expansion lists:
 *     "Indexing — 1,240 of 5,000", "Downloading dfn5b — 62%", "Listening…"
 *   · pulsing — true while anything is actually HAPPENING (ingest/scan, a
 *     model download, streaming speech); the collapsed row breathes gently
 *     off this one flag. Quiet — photography-app restrained.
 *
 * Seat titles never name keys: key hints resolve from the registry
 * (primitives/tooltip.ts), so a keymap change (the mic key is moving)
 * can never strand stale copy here.
 */
import type { IngestStatus, RuntimeStatus } from "../types/dto";
import type { MicState, SegmentTone } from "../actions/types";
import { KIND_LABELS } from "./jobs";
import { scopeLabel } from "./scope";

export interface StationInput {
  ingest: IngestStatus;
  /** Latest RUNTIME snapshot; null = backend dark (tests/dev). */
  runtime: RuntimeStatus | null;
  micState: MicState;
  /** §8.3 existence gate: the mic seat exists only when true (or degraded). */
  asrReady: boolean;
  /** §5.4: the in-flight utterance's BOUND scope; null = nothing streams. */
  streaming: { kind: string; count: number } | null;
}

export interface StationSeat {
  id: "mic" | "search" | "info" | "note";
  /** The registry row this seat dispatches — seats carry registry actions
   * ONLY (resolveAction gates availability; zero new verb paths). */
  actionId: "mic-press" | "open-search" | "toggle-station-detail" | "summon-note";
  /** resolveAction arg for the row. The mic seat resolves mic-press with
   * "toggle": a click IS a tap, so the pointer form keeps plain toggle
   * semantics while the key runs the tap-vs-hold machine (michold.ts). */
  arg?: string;
  /** Lucide glyph name (no emoji, ever — founder rule). */
  icon: "mic" | "mic-off" | "search" | "info" | "pencil";
  /** Accessible label / hover line. NO key names (see module header). */
  title: string;
  /** Quiet rendering hint (UI §7.3): dimmed / breathing. */
  tone?: SegmentTone;
}

export interface StationActivity {
  /** Stable per-source id (keyed each + dedupe by construction). */
  id: string;
  kind: "ingest" | "digest" | "download" | "download-failed" | "mic" | "utterance";
  text: string;
  /** Quiet second clause (error counts, retry hints). */
  hint?: string;
  /** 0..1 when the row has measurable progress. */
  fraction?: number;
}

export interface StationModel {
  pulsing: boolean;
  seats: StationSeat[];
  activities: StationActivity[];
}

/** Queue-spelled pass names read like sentences in the expansion. */
function sentence(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** The mic seat mirrors the mode segment's state mapping (actions/modes.ts
 * — CAPTURE §6.4/§11): absent until ASR exists, dimmed when disarmed/arming,
 * solid armed, breathing while speech is detected, muted-mic degraded. */
function micSeat(micState: MicState, asrReady: boolean): StationSeat | null {
  const base = { id: "mic", actionId: "mic-press", arg: "toggle" } as const;
  switch (micState) {
    case "armedIdle":
      return {
        ...base,
        icon: "mic",
        title:
          "Listening - audio is transcribed on this device and never written to disk.",
      };
    case "armedSpeaking":
      return {
        ...base,
        icon: "mic",
        title:
          "Listening - audio is transcribed on this device and never written to disk.",
        tone: "live",
      };
    case "arming":
      return { ...base, icon: "mic", title: "Arming microphone…", tone: "dim" };
    case "disarmedError":
      return {
        ...base,
        icon: "mic-off",
        title: "Voice capture unavailable - typed notes and pencil still work.",
        tone: "dim",
      };
    default:
      // Disarmed: a dimmed glyph once ASR exists; absent before then.
      return asrReady
        ? { ...base, icon: "mic", title: "Microphone off", tone: "dim" }
        : null;
  }
}

export function stationModel(input: StationInput): StationModel {
  const activities: StationActivity[] = [];

  // Ingest headline: counts while the total is known; an honest
  // "scanning…" while discovery hasn't sized the work yet.
  const ing = input.ingest;
  if (ing.running) {
    if (ing.total > 0) {
      activities.push({
        id: "ingest",
        kind: "ingest",
        text: `Indexing - ${ing.done.toLocaleString()} of ${ing.total.toLocaleString()}`,
        hint: ing.errors > 0 ? `${ing.errors.toLocaleString()} errors` : undefined,
        fraction: Math.min(1, ing.done / ing.total),
      });
    } else {
      activities.push({ id: "ingest", kind: "ingest", text: "Indexing - scanning…" });
    }
  }

  // Digest breakdown: one row per still-queued pass kind. ingest_passes is
  // THE register for background digestion (logic/jobs.ts), so rebuilds,
  // doctor re-pends, and embedding backfills all surface here by
  // construction — reviewer words via the shared label map.
  for (const p of ing.passes) {
    if (p.remaining === 0) continue;
    activities.push({
      id: `digest:${p.name}`,
      kind: "digest",
      text: `${sentence(KIND_LABELS[p.name] ?? p.name)} - ${p.remaining.toLocaleString()} remaining`,
    });
  }

  // Model downloads (RUNTIME §5.2): progress rows while downloading, an
  // honest failed row with the retry hint — never a toast.
  let downloading = false;
  for (const m of input.runtime?.models ?? []) {
    if (m.state === "downloading") {
      downloading = true;
      const pct =
        m.totalBytes > 0 ? Math.floor((m.downloadedBytes / m.totalBytes) * 100) : 0;
      activities.push({
        id: `download:${m.id}`,
        kind: "download",
        text: `Downloading ${m.id} - ${pct}%`,
        // The pump keeps state=downloading across retries; `error` carries
        // the retry detail when one is in flight.
        hint: m.error ?? undefined,
        fraction: m.totalBytes > 0 ? m.downloadedBytes / m.totalBytes : 0,
      });
    } else if (m.state === "failed") {
      activities.push({
        id: `download:${m.id}`,
        kind: "download-failed",
        text: `Download failed - ${m.id}`,
        hint: m.error ?? "retry from Settings",
      });
    }
  }

  // Mic evidence (CAPTURE §11 lives HERE, never in a toast).
  switch (input.micState) {
    case "arming":
      activities.push({ id: "mic", kind: "mic", text: "Arming microphone…" });
      break;
    case "armedIdle":
      activities.push({ id: "mic", kind: "mic", text: "Listening…" });
      break;
    case "armedSpeaking":
      activities.push({ id: "mic", kind: "mic", text: "Listening - speech detected" });
      break;
    case "disarmedError":
      activities.push({
        id: "mic",
        kind: "mic",
        text: "Voice capture unavailable - typed notes and pencil still work.",
      });
      break;
    default:
      break; // disarmed: nothing is happening
  }

  // §5.4: words in flight name where they land (the scope row carries the
  // tether glyph; this row says it in words).
  if (input.streaming !== null) {
    activities.push({
      id: "utterance",
      kind: "utterance",
      text: `Capturing - words land on ● ${scopeLabel(input.streaming.kind, input.streaming.count)}`,
    });
  }

  // The founder's pulse list, exactly: ingest/scan, a model download,
  // streaming speech. Armed-but-idle does NOT pulse — listening quietly is
  // not "happening", and a permanent pulse would train the eye to ignore it.
  const pulsing =
    ing.running ||
    downloading ||
    input.streaming !== null ||
    input.micState === "armedSpeaking";

  // Seats, fixed order: mic (state-aware, existence-gated) · magnifier ·
  // info dot (only when there is something to tell) · note pencil.
  const seats: StationSeat[] = [];
  const mic = micSeat(input.micState, input.asrReady);
  if (mic !== null) seats.push(mic);
  seats.push({ id: "search", actionId: "open-search", icon: "search", title: "Search" });
  if (activities.length > 0) {
    seats.push({
      id: "info",
      actionId: "toggle-station-detail",
      icon: "info",
      title: "What's happening - click to pin the detail open",
    });
  }
  seats.push({ id: "note", actionId: "summon-note", icon: "pencil", title: "Write a note" });

  return { pulsing, seats, activities };
}

// ---------------------------------------------------------------------------
// The pop move — events POP from the station (founder: note creation
// already does this, "which is cool"; mic arm/disarm and utterance
// finalization join it). Pure transition → chip texts; the shell pushes
// them and Station.svelte flashes a short rising chip. No toasts, no sound.
// ---------------------------------------------------------------------------

export interface PopSnapshot {
  mic: MicState;
  /** An utterance is streaming (push-to-talk held / words in flight). */
  streaming: boolean;
}

function armed(s: MicState): boolean {
  return s === "armedIdle" || s === "armedSpeaking";
}

/** Chips for an indicator-state transition. Deliberately narrow: a clean
 * arm, a clean disarm, and an utterance finalizing (the push-to-talk
 * release). Error states do NOT pop — the seat glyph and the activity row
 * already tell that story, and a popping failure would read as alarm. */
export function transitionPops(prev: PopSnapshot, next: PopSnapshot): string[] {
  const out: string[] = [];
  if (!armed(prev.mic) && armed(next.mic)) out.push("Mic armed");
  if (armed(prev.mic) && next.mic === "disarmed") out.push("Mic off");
  if (prev.streaming && !next.streaming) out.push("Captured");
  return out;
}
