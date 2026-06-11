/**
 * Visible modes (featureset §0: "no invisible modes") — every sticky state
 * is a ModeDef whose segment renders in the indicator strip via
 * logic/segments.ts. Auto-advance ships in P4.2; "pencil" (M2a) and "mic"
 * (M2b) are reserved here NOW so their indicator seats exist by
 * construction — their ctx fields stay falsy until their packets.
 */
import type { ModeDef } from "./types";

export const MODES: readonly ModeDef[] = [
  {
    id: "auto-advance",
    isOn: (ctx) => ctx.autoAdvance,
    segment: (ctx) =>
      ctx.autoAdvance ? { text: "A▸", title: "Auto-advance on (A toggles)" } : null,
  },
  {
    id: "pencil", // live since P5.1: the cursor dot + this segment are the
    // ENTIRE mode announcement (UI §4.4 — zero added chrome)
    isOn: (ctx) => ctx.pencilMode,
    segment: (ctx) =>
      ctx.pencilMode ? { text: "✎", title: "Pencil mode (B toggles)" } : null,
  },
  {
    id: "mic", // M2b: recording state lives HERE, never in a toast (§4).
    // Renders CAPTURE §6.4/§11 (P6.1): absent until ASR exists (UI §7.3 —
    // the glyph "appears silently"); dimmed when disarmed; solid armed;
    // `live` tone = the breathing affordance while speech is detected;
    // the muted-mic degraded glyph when the ASR is down — with the §7.3
    // one-line hover copy. Never a modal, never a toast, never text
    // content (partials are debug-panel-only).
    isOn: (ctx) => ctx.micState === "armedIdle" || ctx.micState === "armedSpeaking",
    segment: (ctx) => {
      switch (ctx.micState) {
        case "armedIdle":
          return {
            text: "🎙",
            title:
              "Listening — audio is transcribed on this device and never written to disk.",
          };
        case "armedSpeaking":
          return {
            text: "🎙",
            title:
              "Listening — audio is transcribed on this device and never written to disk.",
            tone: "live",
          };
        case "arming":
          return { text: "🎙", title: "Arming microphone…", tone: "dim" };
        case "disarmedError":
          return {
            text: "🎙⃠",
            title: "Voice capture unavailable — typed notes and pencil still work.",
            tone: "dim",
          };
        default:
          // Disarmed: a dimmed glyph once ASR exists; absent before then.
          return ctx.asrReady
            ? { text: "🎙", title: "Microphone off (M arms)", tone: "dim" }
            : null;
      }
    },
  },
];
