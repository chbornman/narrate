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

/**
 * Station 2.0 transient icons (DESIGN-STATION.md): icons that fade in and
 * gently pulse ONLY while their thing is live, then retire. Distinct from
 * the always-on seats — these come and go with the work. Each is read-only
 * (no registry action): the seats are the click targets, transients are
 * pure status the eye reads at a glance. The optional `count`/`fraction`
 * drive the ingest/digest badge + progress arc.
 */
export interface StationTransient {
  /** One per source: ingest/digest collapse to a single "work" transient
   * (with the count badge + arc); embed, download, and missing-model are
   * their own icons. Stable id keys the {#each} so a fade-out animates. */
  id: "work" | "embed" | "download" | "missing";
  /** Lucide glyph name (no emoji, ever — founder rule). */
  icon: "loader" | "sparkles" | "download" | "triangle-alert";
  /** Accessible label / hover line (no key names). */
  title: string;
  /** A small badge number on the icon (ingest discovered count, queued
   * passes); omitted when there is nothing to count. */
  count?: number;
  /** 0..1 for the progress arc/ring; omitted when the work is unsized. */
  fraction?: number;
  /** The amber warning surface (missing model) reads differently from the
   * cool "working" transients — the component tints accordingly. */
  warn?: boolean;
}

/**
 * The collapsed pill's border color = the most salient active state
 * (DESIGN-STATION.md priority order). Resolved purely so it is unit-tested
 * across every combination; the component maps each to a token.
 *   · "mic"     — mic armed (recording / push-to-talk): red, highest
 *   · "error"   — a failed download OR a needed-but-missing model: amber
 *   · "working" — background work (ingest / digest / embed / download): cool
 *   · "none"    — idle: no border (neutral)
 */
export type BorderState = "mic" | "error" | "working" | "none";

/**
 * A model a feature needs but which is not installed (DESIGN-STATION.md
 * "missing-model prompts, first-class"). Derived from the runtime model
 * registry: a role the app relies on whose row is anything but installed.
 * Surfaced as the amber transient + an inline hover prompt that can resolve
 * it (download, or accept-license-first when the model gates on a license).
 */
export interface MissingModel {
  /** The model id to hand the download/accept-license commands. */
  id: string;
  /** The feature this model unlocks, spelled for the prompt: "Semantic
   * search needs the CLIP model". */
  feature: string;
  /** "the CLIP model" — the human name in the prompt sentence. */
  modelLabel: string;
  /** Download size for the prompt, e.g. "1.2 GB"; omitted when unknown. */
  sizeLabel?: string;
  /** True when the model gates on a license the user has not accepted yet:
   * the prompt offers "Accept license" before "Download". */
  needsLicense: boolean;
}

export interface StationModel {
  pulsing: boolean;
  seats: StationSeat[];
  activities: StationActivity[];
  /** Station 2.0: the transient icons live RIGHT NOW (component fades/pulses
   * them, retiring on removal). Derived from the same evented state as the
   * activities — a rendering view, no new capture machinery. */
  transients: StationTransient[];
  /** Station 2.0: the collapsed pill's border color, by priority. */
  border: BorderState;
  /** Station 2.0: needed-but-missing models, surfaced first-class in the
   * hover prompt (and as the amber `missing` transient + border). */
  missingModels: MissingModel[];
}

// ---------------------------------------------------------------------------
// Station 2.0: missing-model detection (DESIGN-STATION.md)
//
// A feature silently breaks today when its model is absent; the Station is
// where that surfaces and resolves. The mapping below is the ONE place that
// knows "this role unlocks this feature" — derived from the runtime model
// registry's `role`, so a manifest change flows through without new wiring.
// Only roles the app actively relies on appear; "*-alt" roles are optional
// substitutes (never "missing"), so they are deliberately excluded.
// ---------------------------------------------------------------------------

/** Roles whose absence breaks a user-facing feature, with the prompt copy.
 * `role` matches ModelRowDto.role (manifest.rs). No em-dashes (gate). */
const NEEDED_ROLES: Record<string, { feature: string; modelLabel: string }> = {
  // The DFN5B CLIP embedder (manifest.rs role "embedder"): without it the
  // image vectors never build, so semantic search and find-similar are dark.
  embedder: { feature: "Semantic search needs the CLIP model", modelLabel: "the CLIP model" },
  // The text embedder (EmbeddingGemma): the query side of semantic search.
  "text-embedder": {
    feature: "Semantic search needs the text model",
    modelLabel: "the text model",
  },
};

/** A row counts as "present/in-flight" (NOT missing) when it is installed or
 * actively downloading — those have their own transients. Everything else
 * for a NEEDED role (not-downloaded / not-offered / failed / unpinned) is a
 * gap the Station names. */
function modelPresent(state: string): boolean {
  return state === "installed" || state === "downloading";
}

/** "1.2 GB" / "316 MB" from a byte total (download-size prompt copy). */
function sizeLabel(bytes: number): string | undefined {
  if (bytes <= 0) return undefined;
  const gb = bytes / 1_000_000_000;
  if (gb >= 1) return `${(Math.round(gb * 10) / 10).toFixed(1)} GB`;
  return `${Math.round(bytes / 1_000_000)} MB`;
}

/** The needed-but-absent models from a runtime snapshot (null = backend
 * dark: no registry, so nothing is "missing" — the quiet test/dev default). */
export function missingModels(runtime: RuntimeStatus | null): MissingModel[] {
  if (runtime === null) return [];
  const out: MissingModel[] = [];
  for (const m of runtime.models) {
    const need = NEEDED_ROLES[m.role];
    if (need === undefined) continue;
    if (modelPresent(m.state)) continue;
    out.push({
      id: m.id,
      feature: need.feature,
      modelLabel: need.modelLabel,
      sizeLabel: sizeLabel(m.totalBytes),
      // The model gates on a license the user has not recorded yet: the
      // prompt must offer Accept first (the Settings Models flow, inline).
      needsLicense: m.acceptanceRequired && !m.accepted,
    });
  }
  return out;
}

/**
 * The collapsed pill's border color, by priority (DESIGN-STATION.md). PURE
 * over the resolved StationModel pieces so the priority can be unit-tested
 * across every combination: mic beats error beats working beats idle.
 */
export function borderState(input: {
  micArmed: boolean;
  hasError: boolean;
  working: boolean;
}): BorderState {
  if (input.micArmed) return "mic"; // recording / push-to-talk wins outright
  if (input.hasError) return "error"; // a failed download or a missing model
  if (input.working) return "working"; // ingest / digest / embed / download
  return "none"; // idle: neutral, no border
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
  // construction — reviewer words via the shared label map. Station 2.0
  // also rolls these up into transient icons: embedding passes get their
  // own "embed" icon; every other still-queued pass joins the "work" icon.
  let embedRemaining = 0; // image/text-embedding passes → the embed transient
  let digestRemaining = 0; // every other still-queued pass → the work transient
  for (const p of ing.passes) {
    if (p.remaining === 0) continue;
    if (p.name === "image-embedding" || p.name === "text-embedding")
      embedRemaining += p.remaining;
    else digestRemaining += p.remaining;
    activities.push({
      id: `digest:${p.name}`,
      kind: "digest",
      text: `${sentence(KIND_LABELS[p.name] ?? p.name)} - ${p.remaining.toLocaleString()} remaining`,
    });
  }

  // Model downloads (RUNTIME §5.2): progress rows while downloading, an
  // honest failed row with the retry hint — never a toast.
  let downloading = false;
  let downloadFailed = false; // a "failed" row → the amber/error border
  // Aggregate download progress across all in-flight models, for the
  // download transient's arc (the manager runs one file at a time but the
  // arc reads the overall byte fraction, not a single file's).
  let dlDone = 0;
  let dlTotal = 0;
  for (const m of input.runtime?.models ?? []) {
    if (m.state === "downloading") {
      downloading = true;
      dlDone += m.downloadedBytes;
      dlTotal += m.totalBytes;
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
      downloadFailed = true;
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

  // Station 2.0 transient icons (DESIGN-STATION.md): fade in + pulse only
  // while live, then retire. Derived from the SAME evented state as the
  // activities — a rendering view, never a new signal.
  const transients: StationTransient[] = [];

  // Ingest/digest → ONE "work" icon with a count badge + a progress arc. The
  // badge is the live discovered/total count (or the digest backlog when
  // discovery has sized but counting hasn't started); the arc is done/total.
  if (ing.running || digestRemaining > 0) {
    const sized = ing.running && ing.total > 0;
    transients.push({
      id: "work",
      icon: "loader",
      title:
        ing.running && ing.total > 0
          ? `Indexing - ${ing.done.toLocaleString()} of ${ing.total.toLocaleString()}`
          : ing.running
            ? "Indexing - scanning…"
            : "Finishing up the library",
      // Badge: the still-to-go count the eye wants at a glance — the
      // discovered total while scanning, the remaining digest backlog after.
      count: sized
        ? ing.total
        : ing.running && ing.discovered > 0
          ? ing.discovered
          : digestRemaining > 0
            ? digestRemaining
            : undefined,
      fraction: sized ? Math.min(1, ing.done / ing.total) : undefined,
    });
  }

  // Embedding pass → its own "embed" icon (semantic-search backfill in
  // flight). Separate from ingest so the eye can tell "still building search"
  // from "still indexing files".
  if (embedRemaining > 0) {
    transients.push({
      id: "embed",
      icon: "sparkles",
      title: `Embedding for search - ${embedRemaining.toLocaleString()} remaining`,
      count: embedRemaining,
    });
  }

  // Active downloads → a "download" icon with the aggregate byte arc.
  if (downloading) {
    transients.push({
      id: "download",
      icon: "download",
      title: "Downloading a model",
      fraction: dlTotal > 0 ? dlDone / dlTotal : undefined,
    });
  }

  // Needed-but-missing models → the amber warning icon (DESIGN-STATION.md):
  // a feature is dark until this resolves. The hover prompt offers the fix.
  const missing = missingModels(input.runtime);
  if (missing.length > 0) {
    transients.push({
      id: "missing",
      icon: "triangle-alert",
      title:
        missing.length === 1
          ? missing[0].feature
          : `${missing.length} models needed for full search`,
      count: missing.length > 1 ? missing.length : undefined,
      warn: true,
    });
  }

  // The collapsed pill's border, by priority (mic > error > working > idle).
  const micArmed =
    input.micState === "armedIdle" || input.micState === "armedSpeaking";
  const border = borderState({
    micArmed,
    hasError: downloadFailed || missing.length > 0,
    working: ing.running || embedRemaining > 0 || digestRemaining > 0 || downloading,
  });

  return { pulsing, seats, activities, transients, border, missingModels: missing };
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
