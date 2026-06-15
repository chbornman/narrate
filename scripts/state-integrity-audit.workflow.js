export const meta = {
  name: 'state-integrity-audit',
  description: 'Audit PhotoProof data/state integrity: silent-failure bugs, recovery/reset gaps, missing user warnings, + a living checklist',
  phases: [
    { title: 'Audit', detail: 'one auditor per state class vs the failure-mode matrix' },
    { title: 'Verify', detail: 'adversarially confirm each high/silent finding is real' },
    { title: 'Synthesize', detail: 'prioritized report + living checklist doc' },
  ],
}

const MATRIX = [
  'For YOUR state class, walk these FAILURE MODES and report concrete findings:',
  '- corruption (malformed/truncated file, bad bytes, invalid UTF-8)',
  '- partial write / crash mid-operation (atomic? temp+rename? WAL?)',
  '- version skew (schema user_version, GENERATOR_VERSION, model_id, PASS_VERSION, dims, manifest vs installed)',
  '- missing / deleted files or rows (user deleted cache, model, db; offline volume)',
  '- disk full / permission denied / read-only volume',
  '- concurrent access (two writers, app already running, locked db)',
  '- config / preference drift (stale value, out-of-range, per-webview divergence)',
  '- stale DERIVED data (cache/vectors not regenerated after their source changed)',
  '',
  'For EACH finding give: failureMode; severity (high|medium|low); silentFailure',
  '(true if the app produces WRONG behavior with NO error/log/warning, like the',
  'recent fp16 CLIP model_id skew that silently zeroed every topic affinity);',
  'currentHandling (quote file:line); gap; recovery (auto|manual|none, can it',
  'self-heal on restart or via a doctor?); userWarned; resetPath (how a user can',
  'clear/rebuild this state today, or "none"); evidence (file path + lines + short',
  'quote); suggestedFix (concrete, minimal).',
  '',
  'Be rigorous and specific. Prefer fewer well-evidenced findings over speculation.',
  'Read the actual code; do not assume. Repo root: /Users/bornman/projects/photoproof',
].join('\n')

const AUDIT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['stateClass', 'findings', 'summary'],
  properties: {
    stateClass: { type: 'string' },
    summary: { type: 'string' },
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['title', 'failureMode', 'severity', 'silentFailure', 'currentHandling', 'gap', 'recovery', 'userWarned', 'resetPath', 'evidence', 'suggestedFix'],
        properties: {
          title: { type: 'string' },
          failureMode: { type: 'string' },
          severity: { type: 'string', 'enum': ['high', 'medium', 'low'] },
          silentFailure: { type: 'boolean' },
          currentHandling: { type: 'string' },
          gap: { type: 'string' },
          recovery: { type: 'string', 'enum': ['auto', 'manual', 'none'] },
          userWarned: { type: 'boolean' },
          resetPath: { type: 'string' },
          evidence: { type: 'string' },
          suggestedFix: { type: 'string' },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['title', 'isReal', 'severity', 'silentFailure', 'reasoning', 'evidence'],
  properties: {
    title: { type: 'string' },
    isReal: { type: 'boolean' },
    severity: { type: 'string', 'enum': ['high', 'medium', 'low'] },
    silentFailure: { type: 'boolean' },
    reasoning: { type: 'string' },
    evidence: { type: 'string' },
  },
}

const CLASSES = [
  { key: 'sqlite-db', label: 'SQLite database (photoproof.db)', focus: 'Schema migrations + user_version, the open path (open_library_connection, EventStore::open), pragmas (WAL/synchronous), crash recovery (ingest::recover_running, retry_errors on startup), corruption detection, concurrent-open / single-writer assumptions, what happens if the db is from a NEWER app version (downgrade), and whether a corrupt db is detected/warned vs panics. Look in crates/photoproof-core/src/store/ and library/mod.rs.' },
  { key: 'preview-cache', label: 'Preview cache (previews/*.webp)', focus: 'GENERATOR_VERSION regen machinery, atomic_write + temp-file sweep, torn/missing artifact handling on the serve + embed paths, disk-full on write, what happens when a user deletes the cache dir while running, the new micro tier, and whether stale derived previews are detected. crates/photoproof-core/src/library/preview.rs, mod.rs, apps/desktop/src-tauri/src/protocol.rs.' },
  { key: 'vector-store', label: 'Vector store (.ppvec files + vectors table)', focus: 'model_id / dims skew (the recent fp16 CLIP silent-zero bug; confirm the fix and look for SIBLING gaps: text embedder swap, reranker, dims change), file header vs db row consistency, corruption/torn-file, compaction + sweep_dead, what happens if a .ppvec file is deleted but the db row remains (or vice versa), and whether mismatches are detected/warned. crates/photoproof-core/src/retrieval/ppvec.rs, library/embedding.rs, topic.rs.' },
  { key: 'models', label: 'Model downloads (models/ dir, manifest, installed.json)', focus: 'Download then verify (hash/size?) then license acceptance then version. Partial/interrupted download, corrupted model file, manifest vs installed.json drift, a model referenced by config but missing on disk, GC of old/unused models, re-download/reset path, and degraded behavior when a model is absent. crates/photoproof-core/src/runtime/ (manifest.rs, plan.rs, process.rs), apps/desktop/src-tauri/src/runtime.rs, ort_runtime.rs.' },
  { key: 'ingest-passes', label: 'Ingest pass queue (ingest_passes)', focus: 'pass_version skew, stuck running rows, error retry caps (MAX_LIFETIME_ATTEMPTS) and whether legit work can be permanently stranded, backfill enqueue, the new model-aware re-pend (repend_passes_for_model); verify it converges and cannot loop, priority/starvation, and offline-volume deferral. crates/photoproof-core/src/library/ingest.rs, embedding.rs, apps/desktop/src-tauri/src/pump.rs.' },
  { key: 'tuning-config', label: 'Tuning / config (tuning.toml)', focus: 'Parse errors, out-of-range values, missing file, range_or_default clamping, what happens on a malformed toml (panic vs fallback vs warn), and config drift vs compiled defaults. crates/photoproof-core/src/tuning.rs, tuning.default.toml.' },
  { key: 'prefs-localstorage', label: 'Frontend prefs / localStorage (pp.*)', focus: 'Per-webview divergence (the theme bug just fixed; look for OTHER prefs with the same cross-window or persistence hazard), corrupt/missing values, parse fallbacks, and migration of pref keys. apps/desktop/src/lib/state/prefs.ts, theme/, and any other localStorage usage across src/lib.' },
  { key: 'sidecars', label: 'Sidecar files (XMP/JSON next to images)', focus: 'Write atomicity, read/parse of a corrupt or foreign sidecar, the known case-only-rename relink path (s02_2 test is a known failing case; assess it), conflict between db and sidecar, and offline-volume handling. Search crates/photoproof-core/src for sidecar.' },
  { key: 'runtime-sidecars', label: 'Runtime IPC servers (pp-asr-server, llama-server)', focus: 'Process crash + restart/supervision, port conflicts, model-layout runtime dispatch (parakeet vs sherpa), readiness/health, what the UI shows when a server is down, and recovery. crates/pp-asr-server/, apps/desktop/src-tauri/src/runtime.rs, pump.rs, and any supervisor code.' },
  { key: 'app-data-dir', label: 'App data dir lifecycle + global reset', focus: 'Is there a coherent reset / clear all data path or per-store clear (clear_preview_cache exists; what else)? OS data-dir conventions (caches regenerable vs precious). First-run init, and what happens if the whole app-data dir is wiped while running or between runs. Cross-cutting; survey commands in apps/desktop/src-tauri/src/commands/ and library/mod.rs.' },
]

phase('Audit')
log('Auditing ' + CLASSES.length + ' state classes against the failure-mode matrix...')

const audited = await pipeline(
  CLASSES,
  (c) => agent(
    'You are auditing the DATA / STATE INTEGRITY of ONE state class in the PhotoProof desktop app (a local-first photo-annotation tool; Rust core + Tauri + Svelte).\n\nYOUR STATE CLASS: ' + c.label + '\nWHERE TO LOOK: ' + c.focus + '\n\n' + MATRIX + '\n\nReturn the structured audit for this state class only.',
    { label: 'audit:' + c.key, phase: 'Audit', agentType: 'Explore', schema: AUDIT_SCHEMA },
  ),
  (audit, c) => {
    if (!audit) return null
    const toVerify = audit.findings.filter((f) => f.severity === 'high' || f.silentFailure)
    if (toVerify.length === 0) {
      return { stateClass: audit.stateClass, summary: audit.summary, findings: audit.findings.map((f) => ({ ...f, verified: true })) }
    }
    const verifyOne = (f) => agent(
      'Adversarially VERIFY this data-integrity finding against the actual PhotoProof code. Try to REFUTE it; default isReal=false unless the code clearly confirms it. Re-read the cited files.\n\nClass: ' + c.label + '\nFinding: ' + f.title + '\nClaim: ' + f.gap + '\nSilent-failure claim: ' + f.silentFailure + '\nCited evidence: ' + f.evidence + '\nCurrent handling claimed: ' + f.currentHandling + '\n\nConfirm or refute, adjust severity/silentFailure if the evidence warrants, and cite file:line.',
      { label: 'verify:' + c.key, phase: 'Verify', agentType: 'Explore', schema: VERDICT_SCHEMA },
    ).then((v) => ({ ...f, verified: v && v.isReal !== false, verdict: v }))
    return parallel(toVerify.map((f) => () => verifyOne(f))).then((checked) => {
      const verifiedTitles = new Set(toVerify.map((f) => f.title))
      const rest = audit.findings.filter((f) => !verifiedTitles.has(f.title)).map((f) => ({ ...f, verified: true }))
      return { stateClass: audit.stateClass, summary: audit.summary, findings: checked.filter(Boolean).concat(rest) }
    })
  },
)

const classes = audited.filter(Boolean)
const allFindings = classes.flatMap((c) => (c.findings || []).map((f) => ({ ...f, stateClass: c.stateClass })))
const confirmed = allFindings.filter((f) => f.verified !== false && (!f.verdict || f.verdict.isReal !== false))
const refuted = allFindings.filter((f) => f.verdict && f.verdict.isReal === false)
log('Audited ' + classes.length + ' classes: ' + allFindings.length + ' findings, ' + confirmed.length + ' confirmed, ' + refuted.length + ' refuted.')

phase('Synthesize')
const refutedBrief = refuted.map((f) => ({ stateClass: f.stateClass, title: f.title, why: f.verdict && f.verdict.reasoning }))
const classSummaries = classes.map((c) => ({ stateClass: c.stateClass, summary: c.summary }))

const report = await agent(
  'You are writing a SINGLE markdown document for the PhotoProof team: a data/state integrity audit + a LIVING CHECKLIST. Audience: the founder/engineer. Tone: precise, no fluff, no em-dashes in any user-facing copy.\n\n' +
  'You are given CONFIRMED findings (JSON) from a multi-agent audit. Produce the document with these sections:\n' +
  '1. "## Summary": 3-6 sentences on overall posture, biggest risks, counts by severity.\n' +
  '2. "## Prioritized findings": a table sorted by (silentFailure desc, severity desc) with columns State class | Finding | Sev | Silent | Recovery | User warned | Reset path | Fix. Terse cells; put file:line in the Finding cell.\n' +
  '3. "## Silent-failure watchlist": EVERY silentFailure=true finding with a one-line why-dangerous and the fix. Top priority.\n' +
  '4. "## Recovery & reset gaps": group findings where recovery is none/manual or resetPath is none; note the missing escape hatch.\n' +
  '5. "## Living checklist": a reusable state-class x failure-mode checkbox matrix, plus a short "when you add a new state class / version constant, verify:" rule list distilled from the findings.\n' +
  '6. "## Refuted / out of scope": brief.\n\n' +
  'Do not invent findings; use only what is provided. Output ONLY the markdown document.\n\n' +
  'CONFIRMED FINDINGS:\n' + JSON.stringify(confirmed, null, 2) + '\n\n' +
  'REFUTED:\n' + JSON.stringify(refutedBrief, null, 2) + '\n\n' +
  'PER-CLASS SUMMARIES:\n' + JSON.stringify(classSummaries, null, 2),
  { label: 'synthesize', phase: 'Synthesize' },
)

return {
  counts: {
    classes: classes.length,
    findings: allFindings.length,
    confirmed: confirmed.length,
    refuted: refuted.length,
    silentFailures: confirmed.filter((f) => f.silentFailure).length,
    high: confirmed.filter((f) => f.severity === 'high').length,
  },
  reportMarkdown: report,
}
