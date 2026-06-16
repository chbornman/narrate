/**
 * The Capture Station model (founder June 2026, digest-visibility split): the
 * bottom-right indicator, now CAPTURE-ONLY. The digest/ingest/embed/download/
 * offline/settled signaling that used to live here moved to the header
 * Library-status indicator (logic/librarystatus.ts) — the digest is a
 * library-wide truth that belongs where the eye rests, not in the capture
 * organ. What remains here is the capture state ALONE:
 *
 *   · seats — the COLLAPSED icon row. Every seat is a clickable verb that
 *     dispatches its registry row via resolveAction (the one-action-table
 *     rule). Founder ruling on the status-vs-launcher tension: ICONS are the
 *     click targets; the expanded body is READ-ONLY context.
 *   · activities — the read-only hover rows, now mic + streaming utterance
 *     only (the digest rows are gone — see the header indicator).
 *   · pulsing — true while CAPTURE is actually happening (streaming speech /
 *     speech detected). Background digest work no longer breathes the Station.
 *   · border — the collapsed pill's edge: mic (recording red) when armed,
 *     none otherwise. The error/working edges left with the digest signaling.
 *
 * Seat titles never name keys: key hints resolve from the registry
 * (primitives/tooltip.ts), so a keymap change can never strand stale copy.
 */
import type { MicState, SegmentTone } from "../actions/types";
import { scopeLabel } from "./scope";

export interface StationInput {
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
  actionId:
    | "mic-press"
    | "open-search"
    | "toggle-station-detail"
    | "summon-note";
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
  kind: "mic" | "utterance";
  text: string;
}

/**
 * The collapsed pill's border color, now capture-only:
 *   · "mic"  — mic armed (recording / push-to-talk): red.
 *   · "none" — not armed: no border (neutral).
 * (The "error"/"working" edges left with the digest signaling — those live on
 * the header Library-status indicator now.)
 */
export type BorderState = "mic" | "none";

export interface StationModel {
  pulsing: boolean;
  seats: StationSeat[];
  activities: StationActivity[];
  /** The collapsed pill's border color (mic when armed, else none). */
  border: BorderState;
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

  // Mic evidence (CAPTURE §11 lives HERE, never in a toast).
  switch (input.micState) {
    case "arming":
      activities.push({ id: "mic", kind: "mic", text: "Arming microphone…" });
      break;
    case "armedIdle":
      activities.push({ id: "mic", kind: "mic", text: "Listening…" });
      break;
    case "armedSpeaking":
      activities.push({
        id: "mic",
        kind: "mic",
        text: "Listening - speech detected",
      });
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

  // Capture pulse: streaming speech / speech detected. Armed-but-idle does NOT
  // pulse — listening quietly is not "happening", and a permanent pulse would
  // train the eye to ignore it. Background digest work no longer pulses the
  // Station (that signaling moved to the header indicator).
  const pulsing =
    input.streaming !== null || input.micState === "armedSpeaking";

  // Seats, fixed order: mic (state-aware, existence-gated) · magnifier ·
  // info dot (only when there is something to tell) · note pencil.
  const seats: StationSeat[] = [];
  const mic = micSeat(input.micState, input.asrReady);
  if (mic !== null) seats.push(mic);
  seats.push({
    id: "search",
    actionId: "open-search",
    icon: "search",
    title: "Search",
  });
  if (activities.length > 0) {
    seats.push({
      id: "info",
      actionId: "toggle-station-detail",
      icon: "info",
      title: "What's happening - click to pin the detail open",
    });
  }
  seats.push({
    id: "note",
    actionId: "summon-note",
    icon: "pencil",
    title: "Write a note",
  });

  // The collapsed pill's border: mic when armed, else none.
  const micArmed =
    input.micState === "armedIdle" || input.micState === "armedSpeaking";
  const border: BorderState = micArmed ? "mic" : "none";

  return { pulsing, seats, activities, border };
}

// ---------------------------------------------------------------------------
// The pop move — events POP from the station (founder: note creation already
// does this; mic arm/disarm and utterance finalization join it). Pure
// transition → chip texts; the shell pushes them and Station.svelte flashes a
// short rising chip. No toasts, no sound.
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
