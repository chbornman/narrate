# PLAN — Gemma 4 + MTP (multi-token prediction) for the LLM seam

Goal: a lossless 1.4-2.98x decode speedup for the `LanguageModel` seam
(query-parse, summaries, captions) by adding Gemma 4 **MTP** (multi-token
prediction) speculative decoding to the vendored `llama-server`.

Status: research + code-side wiring landed AND the shell-side Metal gate is
now implemented + tested (§5a). Activation is blocked on exactly ONE
founder-owned step: **vendoring a post-#24282 `llama.cpp` binary** (the exact
build/validate recipe is in §5a). Nothing here changes the shipped default
path — the plain E2B argv is pinned byte-identical, and Apple Silicon always
runs the plain target.

---

## 1. Lineage verdict — MAINLINE, not the fork

MTP exists in **both** trees, and this is the single most confusing fact in
the landscape, so state it plainly:

- **`ik_llama.cpp` (the fork)** — PR **#1744** (SamuelOliveirads), merged
  **2026-05-10**. This is where MTP first landed and where the headline
  **2.6-2.98x** numbers come from. Flags: `--spec-type mtp`, `-md <drafter>`,
  `--draft-max`, `--draft-p-min`. Bench harness:
  <https://github.com/karany97/llamacpp-gemma4-mtp>.
- **Mainline `ggml-org/llama.cpp`** — PR **#23398** ("llama : add Gemma4
  MTP", am17an), merged **2026-06-07**, for 31B + 26B-A4B dense variants;
  follow-up **#24282** (merged **2026-06-08**) added the `gemma4-assistant`
  drafter architecture and **E2B/E4B** support. Flags differ from the fork:
  **`--spec-type draft-mtp`**, **`--model-draft`/`-md <drafter>`**,
  **`--spec-draft-n-max <1..4>`**, `--spec-draft-device`, `--spec-draft-p-min`.

**Verdict: we vendor MAINLINE.** RUNTIME §3.1 pins per-platform
`llama-server` builds from `ggml-org/llama.cpp`; we keep that tree. The fork
is off-thesis (a second upstream to track, different flag surface, no
per-platform vendoring story). The code-side draft uses the **mainline**
flag spelling.

**Binary requirement (founder-owned step):** vendor a `ggml-org/llama.cpp`
build dated **after 2026-06-08** (post-#24282) for each platform that will
run MTP. Older builds cannot load the `gemma4-assistant` drafter arch and
will fail to start with the MTP flags. The current pinned spike build
(b9590, brew) predates this.

Sources:
- <https://github.com/ggml-org/llama.cpp/pull/23398>
- <https://huggingface.co/unsloth/gemma-4-E2B-it-qat-GGUF/blob/main/MTP/README.md>
- <https://unsloth.ai/docs/models/mtp>
- <https://github.com/karany97/llamacpp-gemma4-mtp>

## 2. The artifacts — base target + a tiny separate drafter

MTP is **not** a single fused model. It is the existing Gemma 4 target GGUF
**plus a small separate "drafter" GGUF** (architecture `gemma4-assistant`)
that shares the target's KV cache. The drafter proposes N tokens; the target
verifies them in one forward pass; only verified tokens are kept — so output
is **byte-identical to plain decoding** (lossless). Unsloth ships the drafter
at each repo root as `mtp-<model>.gguf` (a near-lossless smart Q4_0); recent
mainline auto-detects it next to the target when launched with `-hf`. We pass
it explicitly with `--model-draft` because we load from pinned local paths.

Pinned files (real SHA-256 from HF LFS pointers, sizes verified via HEAD):

| Model | repo @ rev | target | drafter | mmproj |
|---|---|---|---|---|
| **E2B-MTP** | `unsloth/gemma-4-E2B-it-qat-GGUF` @ `db01ae3c` | `gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf` (2.62 GB) | `mtp-gemma-4-E2B-it.gguf` (**59 MB**) | `mmproj-F16.gguf` (986 MB) |
| **26B-A4B-MTP** | `unsloth/gemma-4-26B-A4B-it-qat-GGUF` @ `02749a7b` | `gemma-4-26B-A4B-it-qat-UD-Q4_K_XL.gguf` (14.25 GB) | `mtp-gemma-4-26B-A4B-it.gguf` (252 MB) | `mmproj-F16.gguf` (986 MB) |
| (31B, rejected) | `unsloth/gemma-4-31B-it-qat-GGUF` @ `365d6571` | `…UD-Q4_K_XL.gguf` (**17.3 GB**) | `mtp-…31B…gguf` (280 MB) | — |

The drafter is tiny (59-280 MB) — the disk/VRAM cost of MTP is negligible
next to the target. **An E2B-class (laptop) MTP drafter DOES exist** — this
is the key sizing fact: MTP is not confined to the big 26B/31B models.

## 3. The llama.cpp flags (mainline)

Append to the existing §3.1 launch line **when MTP is active**:

```
--spec-type draft-mtp \
--model-draft {models}/{id}/mtp-….gguf \
--spec-draft-n-max 4
```

`-ngl 99 -fa on` (flash-attention) as usual. `--spec-draft-n-max` is the
draft ceiling (1-4 tested; 4 is the Unsloth default — good for high-
acceptance CUDA runs). `--reasoning-budget 0` and `--mmproj` stay exactly as
today. Everything else in the launch line is unchanged.

## 4. Per-machine plan — MTP helps the 5080, NOT the laptop

This is the decisive operational finding.

| Machine | Backend | MTP verdict | Pick |
|---|---|---|---|
| **M1 Pro MacBook (16-32 GB)** | Metal | **NET LOSS** — 11% slower at 100% acceptance, up to 28% slower at n_max=6; draft-eval overhead on Metal exceeds the speculative gain (ggml-org/llama.cpp **#23752**, closed 2026-05-27, no fix). | Keep the shipped **`gemma-4-e2b-it-qat-q4_0`** (no MTP). |
| **Ryzen 9900X + RTX 5080 (16 GB)** | CUDA | **WIN** — lossless 1.4-2.98x; E2B ~0.45 / dense ~0.70 draft acceptance; 12B target 52→162 tok/s on B200. | Tier-2 upgrade: **`gemma-4-26b-a4b-it-qat-q4_k_xl-mtp`** (MoE A4B, 14.25 GB fits 16 GB VRAM with KV + the 252 MB drafter + partial CPU offload). E2B-MTP also offered for a lighter, very-fast option. |

Why **not** 31B on the 5080: its Q4_K_XL target is **17.3 GB**, over the
16 GB VRAM budget before KV cache and the projector. 26B-A4B (active 4B, MoE,
tolerates partial CPU offload per RUNTIME §6.2) is the correct quality pick.

Mapping onto the tier system (RUNTIME §6.2): both MTP entries are
**`llm-alt`, tier-2-only** — exactly the existing `gemma-4-e4b-it-q4_k_m`
precedent. The tier-1 floor (the laptop) is untouched; the MTP entries are
*offered*, never auto-applied (§6.2: "Offered, never auto-applied").

**The Metal gate (NOW WIRED — see §5a).** The pure argv builder
`runtime::launch::llama_server_args_mtp` takes `Option<&MtpDraft>`. The
supervisor resolves that option in `supervisors::mtp_draft_for`:
- macOS / Apple Silicon (Metal) → **`None`** (strip MTP, run the plain
  target) regardless of the model id named, per #23752.
- CUDA / Vulkan + the model entry ships an `mtp-` file → `Some(MtpDraft{
  draft_model, n_max: 4 })`.

So even if a Mac config names an MTP model id, it safely runs the plain
target — no failure, no slowdown.

## 5. The SAFE code-side draft (landed on this branch)

NOT touched: the vendored `llama-server` binary (platform-specific,
founder-owned) and the shell-side Metal-gate resolution (lives in the Tauri
shell, alongside the binary update).

Landed:

1. **`crates/photoproof-core/src/runtime/manifest.rs`** — two new
   `ModelEntry` rows (`gemma-4-e2b-it-qat-q4_k_xl-mtp`,
   `gemma-4-26b-a4b-it-qat-q4_k_xl-mtp`), `role: llm-alt`, `tiers: [2]`, real
   SHA-256 pins for target + drafter + mmproj, Gemma terms gate. Test
   `mtp_llm_variants_are_pinned_tier2_and_ship_a_drafter` guards them; the
   tier-1 pinned-sum assertion is unchanged (MTP is tier-2-only).
2. **`crates/photoproof-core/src/runtime/launch.rs`** — `MtpDraft` struct +
   `llama_server_args_mtp(.., Option<&MtpDraft>)`. The old
   `llama_server_args` becomes a back-compat wrapper passing `None`, so every
   existing caller and the shipped E2B path are byte-for-byte identical
   (pinned by test `mtp_none_is_identical_to_the_legacy_argv`). When `Some`,
   appends the mainline MTP flags (pinned by `mtp_some_appends_the_draft_mtp_flags`).
3. **`crates/photoproof-connectors/src/config.rs`** — doc note: the MTP ids
   are config-selectable; no model-pick logic changes (any manifest id flows
   through the existing plan; tier/install gating already enforced).

## 5a. What is now CODE-WIRED vs what the founder must do (the binary)

The Metal gate is no longer "founder-owned shell wiring TODO" — it is
implemented and tested in the supervisor. Activation is now blocked on ONE
thing: vendoring the post-#24282 `llama-server` binary (the recipe is below).

**CODE-WIRED (done on this branch, `cargo fmt`/`clippy`/`test` green):**

1. **The Metal gate** — `apps/desktop/src-tauri/src/supervisors.rs`,
   `mtp_draft_for(entry, dir) -> Option<MtpDraft>`:
   - `cfg!(target_os = "macos")` => `None`. Apple Silicon strips MTP
     regardless of the model id named (the plain target runs; no failure, no
     11-28% Metal slowdown, #23752). The macOS build never even checks for
     the drafter file.
   - non-Apple (CUDA / Vulkan) AND the chosen entry ships an `mtp-*.gguf`
     drafter => `Some(MtpDraft{ draft_model, n_max: 4 })`.
   - A model with NO `mtp-` file (the plain E2B default, E4B) => `None` on
     every platform — byte-identical legacy argv.
   `llama_spec` now calls `launch::llama_server_args_mtp(.., mtp.as_ref())`
   and excludes the `mtp-` drafter from the TARGET-gguf predicate (the
   `*-mtp` entries ship target + drafter + mmproj side by side).

2. **The model-pick / manifest tier** — the two `*-mtp` entries are OFFERED
   at `tiers: [2]`, `role: llm-alt` ("offered, never auto-applied", RUNTIME
   §6.2). The discrete-GPU box (RTX 5080 = tier 2) is the only place they
   appear in a consent sum; selecting one is a `config.llm.model` edit, which
   the runtime plan already gates by tier+install. No per-tier auto-default
   was added — that would violate the llm-alt precedent; the existing
   mechanism (offer at the tier, user/config selects) is the intended pick.

3. **Tests pinning the wiring** (all green on macOS, where the gate fires
   `None`; the non-Apple branches are asserted under `cfg!(not(macos))`):
   - `the_plain_e2b_default_argv_is_byte_identical_to_the_legacy_path` — the
     shipped default's argv equals `llama_server_args(..)` exactly.
   - `mtp_draft_for_resolves_the_pinned_drafter_when_offered` — drafter
     resolution + the Metal short-circuit.
   - `llama_spec_gates_mtp_flags_by_platform` — whole-path: target is the
     Q4_K_XL (never the drafter), MTP flags present iff non-Apple.

**FOUNDER MUST DO — the blocker to activation: vendor the binary.**

Nothing lights MTP up until a post-#24282 `llama-server` sits where the
supervisor finds it. The current pinned spike build (brew, b9590) predates
#24282 and will FAIL to load the `gemma4-assistant` drafter arch (fails
closed: the server refuses to start, the supervisor degrades the LLM
feature, journaling is unaffected — RUNTIME §7). Exact recipe:

```bash
# On margo (Arch, Ryzen 9900X + RTX 5080, CUDA 13.3) — the CUDA build that
# proves the drafter arch loads. Do this on each platform that runs MTP.
ssh caleb@margo.local 'bash -lc "
  set -e
  cd ~/src
  rm -rf llama.cpp && git clone https://github.com/ggml-org/llama.cpp
  cd llama.cpp
  # MUST be after 2026-06-08 (post-#24282 = the gemma4-assistant drafter
  # arch + E2B/E4B support). Pin the exact commit you vendor.
  git log --oneline -1   # record this SHA in the manifest/vendoring notes
  cmake -B build -DGGML_CUDA=ON -DCMAKE_BUILD_TYPE=Release
  cmake --build build --config Release -j --target llama-server
  ./build/bin/llama-server --version   # sanity
"'
```

Validate it actually loads the drafter (this is the go/no-go for the
binary), then place/vendor it:

```bash
# Smoke test: the drafter arch must load and the MTP flags must be accepted.
./build/bin/llama-server \
  --model    <models>/gemma-4-e2b-it-qat-q4_k_xl-mtp/gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf \
  --model-draft <models>/gemma-4-e2b-it-qat-q4_k_xl-mtp/mtp-gemma-4-E2B-it.gguf \
  --spec-type draft-mtp --spec-draft-n-max 4 \
  --mmproj   <models>/gemma-4-e2b-it-qat-q4_k_xl-mtp/mmproj-F16.gguf \
  -ngl 99 -fa on --reasoning-budget 0 --port 8080
# Must reach /health Ready and log a draft acceptance rate (~0.45 E2B /
# ~0.70 dense). Then the supervisor finds it via:
#   1. beside the app executable (bundle: ship it as a resource sibling), or
#   2. on PATH (dev): copy build/bin/llama-server somewhere on $PATH.
```

Per RUNTIME §3.1 the binary is vendored per-platform; this is a VERSION BUMP
of that existing machinery, not new infrastructure. macOS does NOT need a new
binary for MTP (the gate strips it there) — only the CUDA/Vulkan platforms
that will actually run a `*-mtp` model. Once a validated post-#24282 binary
is in place, MTP is live the next `apply_supervisor_plan` converge: select a
`*-mtp` id in `config.llm.model` on the tier-2 box and the supervisor emits
the `--spec-type draft-mtp` argv automatically.

## 6. Expected speedup

Lossless by construction (the target verifies every drafted token).

- **5080 / CUDA**: 1.4-2.98x decode throughput. Acceptance ~0.70 dense,
  ~0.45 E2B; 12B 52→162 tok/s (3.1x) on B200, 26B/31B see the largest dense
  gains (>1.4x, up to 2.98x on the ik_llama harness). The win shows up most
  on summaries/captions (long generations); query-parse is short so the
  prompt-processing share dilutes the gain.
- **M1 Pro / Metal**: **negative** — do not enable (gate strips it).

## 7. Validation (via the existing benches)

The P6.3 harness already measures exactly the right things. Re-run it
per-machine with the MTP entry:

- **`docs/SPIKE-P6.3.md` LLM bake-off table** (spawn→Ready, RSS, **gen
  tok/s**, the 50-query JSON-schema probe, s/query). Add an MTP row on
  margo (RTX 5080) vs the plain target — gen tok/s is the headline; the
  schema probe must stay **50/50** (lossless ⇒ identical output, so it
  must). Compare s/query against §9's 2 s interactive budget.
- **margo build/test loop** (CLAUDE.md): `ssh caleb@margo.local`, pull,
  build the connectors CUDA path, run the spike harness with the vendored
  post-#24282 binary. This is also where the binary gets validated.
- **Concurrency (§12.4)**: MTP changes generation timing; re-confirm the
  ASR-CPU + interactive-LLM + embedding-batch priorities still hold.
- **Acceptance-rate sanity**: log the draft acceptance the server reports;
  if it drops far below ~0.45 (E2B) / ~0.70 (dense), lower `--spec-draft-n-max`.

Gate to flip the default on the 5080: MTP row shows ≥1.4x gen tok/s **and**
50/50 schema validity **and** the parse stays under the §9 budget.

## 8. Effort / risk

- **Code (done)**: LOW. Additive manifest rows + an optional arg + a config
  note; the legacy path is provably unchanged (back-compat wrapper + test).
- **Binary vendoring (founder)**: MEDIUM. Build/pin a post-#24282
  `llama-server` per platform; validate it loads the `gemma4-assistant`
  drafter on CUDA. The existing per-platform vendoring machinery already
  exists (RUNTIME §3.1) — this is a version bump, not new infrastructure.
- **Shell Metal gate (founder)**: LOW. One platform check resolving the
  `Option<MtpDraft>` to `None` on Apple Silicon.
- **Risk**: LOW and well-contained. MTP is lossless by construction; it is
  tier-2 / llm-alt / opt-in; the laptop and the shipped default are
  untouched; a Mac that names an MTP id safely degrades to the plain target.
  The only real risk is a too-old vendored binary, which fails closed (the
  server refuses to start, the supervisor degrades the feature, journaling
  is unaffected per RUNTIME §7).

## 9. Watch triggers (MODELS.md style)

- Mainline llama.cpp closing the **Metal** MTP-overhead gap (#23752 reopened
  / a follow-up) → re-evaluate enabling MTP on the laptop.
- Unsloth re-pinning the QAT GGUF repos (revisions above are pinned; a bump
  needs new SHAs).
- A fused-MTP (single-file) Gemma 4 export that drops the separate drafter.
