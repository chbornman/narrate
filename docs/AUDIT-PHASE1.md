# Phase 1 Adversarial Audit — State of Record

Multi-lens adversarial audit of the committed Phase 1 packets (P1.1 events
engine `ddc77bd`, P1.2 connectors `f604666`) against the normative specs.
Method: 5 independent review lenses (events conformance, invariant-test
quality, core correctness, connectors conformance, canonical-bytes attack)
→ every raw finding judged by 3 independent refuters (spec-text,
code-behavior, materiality angles); a finding is **confirmed** only if ≥2
refuters fail to kill it.

**Status: partially complete.** First run (workflow `wf_a2535d35-43b`,
95 agents) produced 30 raw findings; a session rate-limit killed 47
verifier agents mid-run, leaving 16 findings unverified. A resumed run
(same run id, cached prefix) is re-verifying those. This file is the
durable record; update it when the resume completes, then close out via
the fix packet (see "Disposition").

## Confirmed findings (survived 3-refuter adversarial verification)

### A1 — CRITICAL: within-batch merge dedupe can resurrect redacted plaintext, order-dependently
- Where: `crates/photoproof-core/src/store/mod.rs:542-548` (dedupe), `:1234-1237` (`structural_mismatch`); test gap at `tests/invariants_events.rs:804`
- Spec: EVENTS.md §2.3 (dedupe rule), §8 (order-independence), invariants I6/I8
- The in-batch dedupe keeps whichever same-id copy appears **first in input
  order** and `structural_mismatch` returns false for a scrubbed/unscrubbed
  pair (no integrity warning). Spec: "keep the first, log an integrity
  warning, **except a scrubbed copy always beats an unscrubbed one**."
- Probe (verified twice independently): `merge([unscrubbed E, scrubbed E])`
  with the redaction event absent inserts **full plaintext** (0 warnings);
  reverse order inserts the scrubbed form. Same set, different result =
  I6 violation + resurrection of redacted content. Trigger scenario: stale
  backup sidecar + truncated current sidecar in one rebuild.
- Note: the local-vs-incoming path (`mod.rs:1337-1350`) implements
  scrubbed-beats-unscrubbed correctly; only the within-batch pre-pass is wrong.

### A2 — MAJOR: small-merge scrub path skips §7 step-8 WAL truncate; scrubbed plaintext lingers in the WAL
- Where: `store/mod.rs:562-568` (≤10k path commits without checkpoint; only the >10k path at `:583` runs it); test near-vacuity at `tests/invariants_events.rs:557`
- Spec: EVENTS.md §7 step 8, §8 step 2, I8
- §8 step 2 mandates scrub-in-place per §7 steps 5–8; step 8 = `wal_checkpoint(TRUNCATE)` after commit. Probe: after a small merge that
  newly scrubs, plaintext sits in `journal.db-wal` until Drop. The I8
  byte-scan test only scans after `drop(store)` (Drop checkpoints), so it
  cannot catch this.

### A3 — minor: §8 step-3 corruption defenses have zero positive test coverage
- `integrity_warnings` only ever asserted empty; no test of structural-mismatch
  "local wins + warn", no test of an incoming scrubbed-FORM event (redacted_by
  set, redaction event absent) against an unscrubbed local row
  (`mod.rs:1343-1350`). Code reads correct; regressions would be silent.

### A4 — minor: I16 query-count assertion has no lower bound (vacuous if folds bypass the traced read pool)
- `tests/invariants_events.rs:1078`: asserts `used <= 3/4`, never `>= 2`.
  Reads routed through the untraced writer connection would count 0 and pass.

### A5 — minor: canonical-JSON rejection-test gaps
- Untested rejections (all currently rejected, several only via the
  byte-equality catch-all at `canonical.rs:252`): duplicate JSON keys, `-0`,
  exponent-form integers, over-width/u64-max integers, lone surrogates,
  BOM, leading whitespace. No property round-trip over arbitrary text.

## Pending re-verification (verifiers killed by rate limit; resume in flight)

Distinct items (duplicates of A1/A2/A5 from other lenses excluded):

| # | Sev (claimed) | Finding |
|---|---|---|
| P1 | critical | `sidecar_dirty` ack race: `since_ts` never advances / `ack_dirty` can delete dirty marks newer than those read — sidecar propagation (incl. queued redactions) silently lost |
| P2 | major | Blocked/busy `wal_checkpoint(TRUNCATE)` result silently discarded everywhere (incl. `redact()`), no retry |
| P3 | major | Interrupted large merge leaves derived tables stale; idempotent re-merge does not heal (dups short-circuit recompute) |
| P4 | minor | `redact()` on a dangling revision chain scrubs only the target, not reachable revision descendants |
| P5 | minor | `append()` never consults the redactions registry |
| P6 | minor | No mechanism for §5.1 operational rules (periodic `PRAGMA optimize`, idle checkpoint) |
| P7 | minor | Retraction-of-redaction invalidity claimed in a comment, never exercised at append or merge |
| P8 | minor | I10 cycle coverage limited to a two-node cycle in a single merge batch (1 of 3 refuters reported back: refuted; under-verified) |

## Refuted (≥2 refutations — do not re-litigate)

- I6 generator excludes duplicate redactions / `dump_truth` reduces `redacted_by` (critical claim — refuted, sanctioned by B7)
- I2 is an example test not a property test (refuted on materiality)
- MockEmbedder lacks failure injection; MockLanguageModel can't model in-flight calls; `TranscriptSegment.onset` can't be absent; MockVad gate/delay interleaving; VecKey kind↔unit coherence (all refuted: out of P1.2's contract or sanctioned)
- UtcMillis not closed under wire format; `parse_payload_json` normalizes DB column text; u32 field widths vs unbounded spec ints (refuted: defensible readings)

## Disposition

Fix packet (coordinator task #12) applies A1–A5 plus whatever survives from
P1–P8, with regression tests, in `src/store/mod.rs` + new/extended invariant
tests — ownership disjoint from in-flight P2.1 (sidecar/) and P2.2 (library/).
Ledger entry goes to docs/BUILD-LOOP.md when the fix commit lands.

Resume artifacts (this machine): run id `wf_a2535d35-43b`; script
`~/.claude/projects/-home-caleb-projects-narrate/905dc663-32e7-4ba1-a897-b4c4e697bf02/workflows/scripts/phase1-spec-audit-wf_a2535d35-43b.js`.
If the resume is lost, P1–P8 above are sufficient to re-verify directly —
do not re-run the full 5-lens sweep.
