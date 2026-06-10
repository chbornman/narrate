# Root Cause Analysis: btrfs Volume Matching & Preview 404s

**Date:** 2026-06-10  
**Severity:** Critical (application non-functional on btrfs)  
**Components:** `photoproof-core/src/library/mod.rs`, `apps/desktop/src/lib/ipc/urls.ts`

## Summary

Two independent bugs rendered the application non-functional on btrfs systems:
1. Volume probe backward-compat logic corrupted mount points, breaking the watcher
2. Preview URL construction used the wrong protocol scheme, causing all thumbnails to 404

## Issue 1: Watcher "No path was found" at launch

### Symptoms
- Error at startup: `photoproof: watcher for <root_id> unavailable at launch: watch error: No path was found`
- All existing roots became inaccessible
- Paths in the database pointed to wrong mount points

### Root Cause

**File:** `crates/photoproof-core/src/library/mod.rs:549-573` (in `probe_volumes()`)

On btrfs, `/` (subvol `@`) and `/home` (subvol `@home`) share the same block device UUID. The new probe (added to fix btrfs subvolume disambiguation) emits subvol-qualified platform IDs like `UUID:/@home`, but existing database rows stored bare UUIDs from before the change.

When `probe_volumes()` matched a bare-UUID row against multiple mount candidates sharing the same UUID, the code **unconditionally preferred the subvol-qualified candidate**:

```rust
// BUG: always prefers qualified, even when stored row is bare
let pick = candidates.iter()
    .find(|i| mounts[**i].platform_id.as_deref().is_some_and(|p| p.contains(':')))
    .or_else(|| candidates.first());
```

This rewrote the volume's `mount_point` to the wrong subvolume (e.g. `/` instead of `/home`), breaking every path reference.

### The Fix

Match the candidate preference to the shape of the known platform ID:

```rust
let known_is_qualified = pid.contains(':');
let pick = if known_is_qualified {
    candidates.iter()
        .find(|i| mounts[**i].platform_id.as_deref().is_some_and(|p| p.contains(':')))
        .or_else(|| candidates.first())
} else {
    candidates.iter()
        .find(|i| mounts[**i].platform_id.as_deref().is_some_and(|p| !p.contains(':')))
        .or_else(|| candidates.first())
};
```

### Impact
- **Affected users:** All btrfs users who upgraded from a version with bare-UUID storage
- **Data corruption:** Database `volumes.mount_point` values were overwritten with wrong subvolumes
- **Recovery:** Delete `~/.local/share/com.photoproof.desktop/photoproof.db` and re-register roots

## Issue 2: Previews 404 (black/blank thumbnails)

### Symptoms
- All thumbnails appeared black/blank
- Network tab showed 404 errors for `photoproof://` URLs
- Preview files existed on disk and were valid WebP images

### Root Cause

**File:** `apps/desktop/src/lib/ipc/urls.ts:9-13`

`convertFileSrc(path, "photoproof")` is Tauri's helper for the built-in **asset** protocol. It produces `http://photoproof.localhost/thumb/<hash>` URLs.

But the Rust side uses `register_asynchronous_uri_scheme_protocol("photoproof", ...)`, which handles `photoproof://localhost/thumb/<hash>` — a different scheme entirely.

The asset protocol URLs never reached the custom handler, so every thumbnail request 404'd.

### The Fix

Replace `convertFileSrc` with hand-built URLs:

```typescript
// BEFORE (wrong — asset protocol)
import { convertFileSrc } from "@tauri-apps/api/core";
export const thumbUrl = (hash: string): string =>
  convertFileSrc(`thumb/${hash}`, "photoproof");

// AFTER (correct — custom scheme)
export const thumbUrl = (hash: string): string =>
  `photoproof://localhost/thumb/${hash}`;
```

### Impact
- **Affected users:** All users (not btrfs-specific)
- **Workaround:** None (previews were completely broken)

## Lessons Learned

1. **Backward compatibility requires symmetric logic:** When upgrading stored data formats, the matching logic must prefer candidates that match the stored format's shape, not unconditionally prefer the new format.

2. **Tauri protocol APIs are not interchangeable:** `convertFileSrc` is for the built-in asset protocol; custom schemes registered with `register_asynchronous_uri_scheme_protocol` require hand-built URLs.

3. **Database migrations need validation:** The mount_point corruption could have been caught with a post-migration check that verifies all active roots still resolve to existing directories.

## Related Commits

- `probe_volumes()` fix: (commit hash TBD)
- Preview URL fix: (commit hash TBD)
