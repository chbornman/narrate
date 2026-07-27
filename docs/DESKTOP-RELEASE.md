# Desktop build and release contract

Photoproof's installed desktop package must be self-contained. A clean target
machine must not need this Cargo workspace, a developer `PATH`, or a separately
installed speech server.

## Supported package recipe

From `apps/desktop`, install the locked frontend dependencies and run:

```text
bun install --frozen-lockfile
bun run bundle
```

`bundle` is the one supported native-package entry point. Before Tauri builds
the shell it:

1. builds `pp-asr-server` in release mode with `engine-parakeet`;
2. derives the Rust target triple and stages the binary at Tauri's required
   sidecar name;
3. builds the production frontend;
4. creates every native bundle supported by the current host.

The generated staging directory is ignored by Git. CI starts from a clean
checkout, builds Linux, macOS, and Windows packages, searches the completed
bundle for the target-native `pp-asr-server`, and rejects an absent or
implausibly small child.

The settled Linux foundation build produced DEB, RPM, and AppImage packages.
Both DEB extraction and the actual AppImage `--appimage-extract-and-run` path
passed installed smoke: schema 16, `Usable` in 7 ms, shutdown in 0 ms, adjacent
ASR sidecar, nine-file backup helper inventory, successful restore, and
retained rollback. The AppImage wrapper sets `NO_STRIP=1` because linuxdeploy's
embedded binutils reject modern `SHT_RELR` sections; Cargo and distribution
libraries are already stripped. AppImage construction currently downloads the
official type-2 runtime, so the build host needs network access.

`llama-server` is intentionally not bundled in this release contract. LLM
features remain dark unless an explicitly supported binary is resolved. It
must not be inferred from a developer `PATH` in an installed build.

## Signing and publication

The three unsigned CI bundles are build evidence, not releases. A production
release additionally requires:

- macOS Developer ID signing and notarization credentials;
- Windows Authenticode credentials;
- a Tauri updater signing key whose private half is held only in the release
  secret store;
- an HTTPS update endpoint and a committed matching updater public key;
- release notes naming schema compatibility and rollback limits.

Never generate or commit a production private key in this repository. Never
publish an unsigned CI artifact as an update. The release job must fail closed
when any signing credential is absent.

Updater rollout is staged: founder machines first, then a small stable cohort,
then general availability. The server may stop offering the new version at any
point. Rollback means publishing a newly signed higher patch version containing
the prior known-good code; clients are never instructed to install an
unsigned or lower-version artifact. Database migration compatibility must be
confirmed before advancing each cohort.

## Installed smoke gate

Each platform gate uses a clean CI checkout and must prove:

- the native package is constructed;
- the ASR sidecar is inside the finished package;
- the app launches without a workspace or manually staged child;
- the health snapshot reaches a usable degraded baseline with no models;
- with the pinned ASR fixture installed, the ASR supervisor reaches Ready;
- quit leaves no child process or database writer.

The unsigned foundation workflow extracts the native package and invokes the
installed executable's `--installed-smoke` lane against a fresh app-data
directory. That lane proves the installed executable:

- finds `pp-asr-server` beside itself, exactly where the runtime resolver
  requires it (including the Windows `.exe` suffix);
- opens and migrates a new database without the source workspace;
- reaches the model-free `Usable` lifecycle baseline;
- reports no child runtime Ready without models;
- follows the normal bounded shutdown path and writes a receipt.

The installed receipt is also a release budget gate: a fresh, model-free
package must reach `Usable` within 5,000 ms and complete shutdown within 5,000
ms on every native CI host. It records the lifecycle subsystem-health snapshot
and prints both timings. These intentionally generous package-level ceilings
catch accidental blocking hardware probes, network-volume work, or detached
shutdown tasks; tighter interactive budgets live in the performance suite.

This is stronger than searching an archive for a filename. It still does not
claim a real WebKit window rendered or that a multi-gigabyte ASR fixture reached
Ready. Those remain founder-machine gates until a pinned, redistribution-safe
fixture and GUI driver are available.

## Production workflow

Only tags shaped `desktop-vX.Y.Z` can start
`.github/workflows/desktop-release.yml`. The workflow targets the protected
`desktop-production` GitHub environment and:

1. requires the workspace, package, Tauri, tag, and release-record versions to
   agree;
2. requires release notes with changes, database compatibility, and rollback,
   and requires the recorded schema to equal the code constant;
3. fails before build if any platform credential, updater key, or rollout
   endpoint is absent;
4. imports Developer ID and Authenticode identities into ephemeral runner
   stores;
5. injects the updater public key and HTTPS endpoint through a runner-temporary
   Tauri config, creates updater artifacts, and signs them with the secret
   private key;
6. verifies updater signatures plus macOS code-signing/notarization stapling or
   Windows Authenticode;
7. runs the extracted installed-package smoke;
8. leaves the GitHub release as a draft.

Unsigned `desktop-foundation.yml` artifacts remain plainly separate build
evidence. They never set `PHOTOPROOF_UPDATES_ENABLED`, never create updater
artifacts, and cannot contact or install from a feed.

## Update UX and safety

The app does not check for updates during startup. Settings shows whether the
build has a signed channel, checks only after an explicit click, and asks again
before installation. Installation re-checks that the feed still offers the
exact version the user approved, downloads and verifies the mandatory Tauri
signature before stopping application writers, then installs and restarts.
Concurrent update operations are rejected.

Developer and unsigned CI builds keep the same IPC/UI shape but return
`disabled`; there is no insecure or unsigned fallback.

## Staged rollout and rollback

The production endpoint must be a cohort-aware HTTPS service. GitHub's static
`latest.json` endpoint is not sufficient for founder, small-cohort, and general
availability stages because it offers one answer to everyone.

1. Keep the GitHub release draft. Offer the signed candidate only to registered
   founder installations and complete clean-machine, launch/quit, model-ready,
   and migration/restore checks.
2. Promote the same immutable signed artifacts to a small stable cohort. Watch
   startup failures, unclean-launch markers, update failures, schema-open
   refusals, and runtime readiness before expansion.
3. Promote to general availability only after the observation window passes.
   Publishing the GitHub draft is a separate explicit action.

At any stage, stop the endpoint from offering the version. If schema migration
occurred, restore the verified pre-upgrade backup before older code opens the
data. The distributable rollback is the prior known-good code rebuilt as a
newly signed, higher patch version. Never lower the version comparator, replace
an artifact in place, or offer unsigned bytes.

## External production blockers

Scaffolding is complete, but a real release correctly remains impossible until
the owner supplies all of these to the protected environment:

- `PHOTOPROOF_UPDATE_ENDPOINT`: cohort-aware HTTPS updater service;
- `PHOTOPROOF_UPDATER_PUBLIC_KEY` plus matching
  `TAURI_SIGNING_PRIVATE_KEY` and password;
- Apple Developer ID `.p12`, its password, ephemeral keychain password, and App
  Store Connect issuer/key/private key for notarization;
- Windows Authenticode `.pfx`, its password, and the chosen timestamp service;
- a redistribution-safe pinned ASR installed-smoke fixture for the Ready gate.

No private key, certificate, endpoint placeholder, or fake success assertion is
committed.

## Evidence state - July 27 2026

- Local unsigned Linux artifacts and DEB/AppImage installed smoke: passed.
- Checked-in Linux/macOS/Windows unsigned and production workflow contracts:
  syntax and local contract validation passed; remote runs pending.
- Installed ASR Ready: pending a pinned redistribution-safe model fixture.
- macOS distribution: blocked on Developer ID Application/notarization
  credentials; the founder Mac currently has only an Apple Development
  identity.
- Windows Authenticode, signed updater publication, staged rollout, and
  rollback drill: external credentials/service and native receipts pending.
