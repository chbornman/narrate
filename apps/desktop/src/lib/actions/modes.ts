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
    id: "pencil", // M2a: lights up when ctx.pencilMode exists for real
    isOn: (ctx) => ctx.pencilMode,
    segment: (ctx) =>
      ctx.pencilMode ? { text: "✎", title: "Pencil mode" } : null,
  },
  {
    id: "mic", // M2b: recording state lives HERE, never in a toast (§4)
    isOn: (ctx) => ctx.micArmed,
    segment: (ctx) => (ctx.micArmed ? { text: "🎙", title: "Listening" } : null),
  },
];
