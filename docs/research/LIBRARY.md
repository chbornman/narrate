# Research: Media-Library Ingest, Identity, Watching, Sidecars

Validation of spec/LIBRARY.md against how Immich, PhotoPrism, digiKam,
darktable, Lightroom, Photo Mechanic, and Syncthing actually behave.

## Verdicts

- **Full-file BLAKE3 as identity — VALIDATED.** Immich uses full-file SHA-1
  as its dedupe key ([duplicates utility](https://docs.immich.app/features/duplicates-utility/),
  [client-side hashing](https://github.com/immich-app/immich/discussions/4154));
  PhotoPrism dedupes by SHA-1+size ([docs](https://docs.photoprism.app/user-guide/library/duplicates/)).
  Neither moved away from full hashing. Counterexample: digiKam's uniqueHash
  is MD5 of first+last 100 KB + size ([digikam-users](https://mail.kde.org/pipermail/digikam-users/2013-June/017748.html)) —
  with known in-place-edit blind spots. Dupe finders tier (czkawka: size →
  2 KB prehash → full — [workflow](https://deepwiki.com/qarmin/czkawka/4.1-tool-types-and-workflow)).
- **1.5 TB first-run hash — VALIDATED as practice, RISKY as a budget**: the
  ≥1 GB/s figure holds on NVMe only; a 120 MB/s USB archive ≈ 3.5 h/TB.
  Resolution applied: budget re-scoped to internal NVMe + slow-volume UX
  requirement (previews trail hashing visibly, framing in the indicator).
- **size+mtime fast path + 2 s FAT tolerance — VALIDATED** (the
  rsync/Syncthing model; exFAT mtime is 2 s-granular and cross-OS
  inconsistent — [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/3840573/why-does-windows-only-have-2-second-resolution-for),
  [exFAT timestamp primer](https://blog.1234n6.com/exfat-timestamps-exfat-primer-and-my-methodology/),
  [forensics study](https://www.sciencedirect.com/science/article/pii/S2666281722001573);
  PhotoPrism's mtime-churn mass-reindex is the failure mode of not tolerating
  it — [discussion #3451](https://github.com/photoprism/photoprism/discussions/3451)).
  Gap fixed: **FAT stores local time → DST shifts every mtime by exactly 1 h**
  → uniform-shift detection added (L2).
- **In-place overwrite = new identity — VALIDATED** (PhotoPrism identical;
  its path-heuristic bugs show the alternative — [issue #568](https://github.com/photoprism/photoprism/issues/568)).
- **Watcher + reconciliation — VALIDATED in structure.** notify's own docs
  concede missed events at scale ([docs.rs/notify](https://docs.rs/notify));
  Syncthing keeps hourly rescans with the watcher on, by explicit decision
  ([docs](https://docs.syncthing.net/users/syncing.html), [forum](https://forum.syncthing.net/t/linux-are-periodic-full-rescans-really-needed-when-fs-watcher-is-enabled/12548));
  Immich labels watching experimental and has shipped watcher regressions
  ([#23824](https://github.com/immich-app/immich/issues/23824), [#20858](https://github.com/immich-app/immich/issues/20858)).
  Gaps fixed: reconcile on **system wake** and on **watcher error/overflow** (L3).
- **Embedded-preview-first — VALIDATED** (it's Photo Mechanic's entire speed
  story — [supported formats](https://docs.camerabits.com/support/solutions/articles/48000361354-supported-file-formats-in-photo-mechanic-6);
  darktable extracts embedded thumbs by default — [manual](https://docs.darktable.org/usermanual/4.0/en/lighttable/digital-asset-management/thumbnails/)).
  Known gaps: older Sony ARW small previews (~1616×1080), CR3 HDR-PQ HEIF
  previews, some DNGs preview-less ([FastRawViewer](https://www.fastrawviewer.com/RawPreviewExtractor)).
  Crates: rawler primary ([docs.rs](https://docs.rs/rawler/latest/rawler/)),
  quickraw exposes thumbnail+orientation ([repo](https://github.com/RawLabo/quickraw)),
  libraw bindings for the long tail. **Correctness fixes applied: preview
  orientation verification (previews are inconsistently pre-rotated —
  [exiftool forum](https://exiftool.org/forum/index.php?topic=4415.0),
  [Wikimedia hit this](https://phabricator.wikimedia.org/T172556)) and the
  preview→decode color-shift / stroke-substrate invariant** (L4, L5).
- **Thumbnails WebP 512/2560 fan-out, no eviction — VALIDATED** (Immich:
  WebP q80 @ 250 px thumb, 1440 px preview — [settings](https://docs.immich.app/administration/system-settings/);
  PhotoPrism hash-pathed cache — [docs](https://docs.photoprism.app/developer-guide/api/thumbnails/);
  AVIF correctly rejected: 2–10× slower encode, trivial savings at thumb
  sizes — [crystallize](https://crystallize.com/blog/avif-vs-webp)). Honest
  size note: 2560 px q87 of detailed photos ≈ 0.8–1.5 MB ⇒ 50–75 GB worst
  case at 50k — report, don't change.
- **Volume identity (marker + platform ids) — VALIDATED**: "no surveyed tool
  does this better."
- **JSON sidecars beside originals — VALIDATED.** Precedent: 20 years of XMP
  ([Lightroom](https://helpx.adobe.com/lightroom-classic/help/create-xmp-acr-files.html),
  [darktable extension-preserving naming](https://docs.darktable.org/usermanual/4.2/en/overview/sidecar-files/sidecar-import/))
  and Google Takeout's `IMG_1234.jpg.supplemental-metadata.json`
  ([explainer](https://metadatafixer.com/learn/google-takeout-json-files-explained)).
  Our full-suffix convention avoids Lightroom's RAW+JPEG `.xmp` collision.
  The real operational risk is **cloud-sync churn** (version spam, metadata
  touching — [Lightroom Queen](https://www.lightroomqueen.com/community/threads/how-to-run-lightroom-from-a-onedrive-sync-folder.25987/),
  [storage review](https://kevinlisota.photography/2019/02/online-storage-review/))
  — fixed by L6.
- **BLAKE3 over xxh3 — VALIDATED, keep BLAKE3**: disk-bound either way; and
  cross-machine sidecar merge makes image_hash a long-lived identifier where
  collision resistance is wanted ([comparison](https://ssojet.com/compare-hashing-algorithms/xxhash-vs-blake3)).

## Amendments (all applied)

- **L1** 90-min budget re-scoped to internal NVMe; slow-volume UX
  requirement (previews trail hashing; progress framing). The provisional
  quick-hash tier was considered and rejected — two-state identity is too
  much complexity for the journal's integrity model.
- **L2** Uniform clock-shift detection: >N% of a root mtime-shifted by the
  same round-hour delta with sizes unchanged ⇒ clock-shift event; update
  mtimes, re-hash a sample to confirm — never a full re-hash storm.
- **L3** Reconciliation also triggers on system wake and on watcher
  error/overflow (immediate scan of that root).
- **L4** Embedded-preview orientation policy: verify preview aspect against
  RAW dimensions/orientation; apply EXIF orientation only if not already
  display-oriented; per-format fixtures (Nikon/Fuji portrait) in acceptance
  tests; quickraw orientation as cross-check.
- **L5** Full-decode backfill MAY alter tone/color, MUST preserve
  display-oriented geometry exactly; never regenerate a stroke substrate
  except via generator_version (made explicit).
- **L6** Cloud-sync awareness: detect Dropbox/OneDrive/Drive/iCloud roots →
  one-time advisory; exclusions for sync caches/partials; **placeholder/
  dataless files detected at stat level and deferred** (hashing one forces
  full hydration); sidecar writes mtime-stable (no rewrite of identical
  content).
- **L7** Threshold-miss-with-pending-strokes images jump the backfill queue;
  CR3 HDR-PQ HEIF previews routed with HEIC through the libheif-capable
  backfill.

## Non-issues

inotify/FSEvents capacity at 50k (a few thousand dirs; horror stories are
node_modules-class) · BLAKE3 throughput · SHA-1 ecosystem compat ·
hash-collision risk · fan-out pressure · WebP decode speed · sidecar-clutter
tolerance (darktable/Takeout precedent) · RAW+JPEG-as-two-images (matches
PhotoPrism's model; stacking is a recorded deferral).
