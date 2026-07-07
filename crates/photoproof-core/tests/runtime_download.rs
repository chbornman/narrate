//! spec/RUNTIME.md §5 download manager against the scripted stub HTTP
//! server with cuttable connections. The §13 criteria covered here:
//! §13.4 (resume from the byte offset across relaunch; corrupted part
//! re-fetched without user action) and §13.7 (the license gate is
//! byte-zero — asserted AT THE SERVER, not the client).

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use photoproof_connectors::mock::{StubHttpServer, StubResponse};
use photoproof_core::runtime::download::sha256_bytes;
use photoproof_core::runtime::{
    Acceptances, DownloadError, DownloadManager, FileEntry, License, ModelEntry, NoPace, Pacer,
    RuntimeBus, RuntimeEvent,
};

fn file_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn model_with(server: &StubHttpServer, files: Vec<(&str, &[u8])>, gated: bool) -> ModelEntry {
    ModelEntry {
        id: "test-model".into(),
        role: "llm".into(),
        tiers: vec![1, 2],
        license: License {
            name: if gated { "Gated Terms" } else { "Apache-2.0" }.into(),
            url: "https://example.test/license".into(),
            acceptance_required: gated,
        },
        total_bytes: files.iter().map(|(_, b)| b.len() as u64).sum(),
        files: files
            .into_iter()
            .map(|(path, bytes)| FileEntry {
                repo: server.base_url(),
                revision: "pinned".into(),
                path: path.into(),
                sha256: sha256_bytes(bytes),
                bytes: bytes.len() as u64,
            })
            .collect(),
    }
}

fn accepted(model: &ModelEntry) -> Acceptances {
    let mut acc = Acceptances::default();
    acc.accept(&model.id, &model.license.url, "2026-06-11T00:00:00Z");
    acc
}

const NOW: &str = "2026-06-11T09:00:00Z";

/// D3: the never-cancelled flag most tests pass — a static so the 20-odd
/// call sites don't each mint a local.
static NO_CANCEL: AtomicBool = AtomicBool::new(false);

#[test]
fn s13_4_cut_mid_download_resumes_from_the_byte_offset_across_relaunch() {
    let server = StubHttpServer::start();
    let payload = file_bytes(200_000, 7);
    let model = model_with(&server, vec![("weights/model.gguf", &payload)], false);
    // First attempt: the server cuts the connection at 80,000 bytes.
    let route = server.route(
        "/weights/model.gguf",
        StubResponse::CutBody {
            status: 200,
            body: payload.clone(),
            cut_after: 80_000,
            extra_headers: vec![],
        },
    );
    // After the "relaunch", it serves Range requests honestly.
    StubHttpServer::push(
        &route,
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect_err("cut connection interrupts");
    assert!(
        matches!(err, DownloadError::Interrupted { got_bytes: 80_000 }),
        "got {err:?}"
    );
    // Path-preserving: the part file lives at the FULL relative path, not
    // the flattened basename (the entry's path is weights/model.gguf).
    let part = dir.path().join("test-model/weights/model.gguf.part");
    assert_eq!(
        std::fs::metadata(&part).unwrap().len(),
        80_000,
        "the part file holds the progress at the nested path"
    );
    assert!(!manager.is_installed("test-model"));

    // Relaunch: a NEW manager instance resumes from the part length.
    let manager2 = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager2
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("resume completes");
    let requests = server.requests_for("/weights/model.gguf");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("Range"), None, "first fetch from 0");
    assert_eq!(
        requests[1].header("Range"),
        Some("bytes=80000-"),
        "§5.2: resume from the part-file length"
    );
    let dest = dir.path().join("test-model/weights/model.gguf");
    assert_eq!(std::fs::read(&dest).unwrap(), payload, "SHA-256 verified");
    assert!(!part.exists(), "atomic rename consumed the part file");
    assert!(manager2.is_installed("test-model"));
    let rec = &manager2.installed()["test-model"];
    assert_eq!(rec.manifest_version, 1);
    assert_eq!(rec.when, NOW);
}

/// §13.7 — THE byte-zero license gate, asserted at the stub server: the
/// request for file content is never issued pre-acceptance.
#[test]
fn s13_7_license_gate_zero_bytes_before_acceptance_asserted_at_the_server() {
    let server = StubHttpServer::start();
    let payload = file_bytes(10_000, 3);
    let model = model_with(&server, vec![("gated.gguf", &payload)], true);
    server.route(
        "/gated.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(
            &model,
            1,
            &Acceptances::default(),
            &mut NoPace,
            &NO_CANCEL,
            NOW,
        )
        .expect_err("gated without acceptance");
    assert!(matches!(err, DownloadError::LicenseNotAccepted { .. }));
    assert!(
        server.requests().is_empty(),
        "ZERO requests issued — not a HEAD, not a byte (§13.7)"
    );
    assert!(
        std::fs::read_dir(dir.path().join("test-model")).is_err(),
        "no on-disk residue either"
    );

    // The recorded acceptance opens the gate.
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("accepted download");
    assert_eq!(server.requests().len(), 1);
    assert!(manager.is_installed("test-model"));
}

/// §13.4: a corrupted part-file is detected (SHA mismatch on completion)
/// and re-fetched WITHOUT user action — once — then surfaces.
#[test]
fn corrupted_part_is_deleted_and_refetched_once_automatically() {
    let server = StubHttpServer::start();
    let payload = file_bytes(120_000, 9);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("test-model")).unwrap();
    // A corrupted part: right length prefix, wrong bytes (disk bitrot /
    // interrupted write).
    std::fs::write(
        dir.path().join("test-model/model.gguf.part"),
        file_bytes(60_000, 250),
    )
    .unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("auto re-fetch saves the day");
    let requests = server.requests_for("/model.gguf");
    assert_eq!(requests.len(), 2, "resume attempt + ONE automatic re-fetch");
    assert_eq!(
        requests[0].header("Range"),
        Some("bytes=60000-"),
        "the corrupted part resumed first (mismatch only provable at completion)"
    );
    assert_eq!(
        requests[1].header("Range"),
        None,
        "the re-fetch starts clean"
    );
    assert_eq!(
        std::fs::read(dir.path().join("test-model/model.gguf")).unwrap(),
        payload
    );
}

#[test]
fn second_checksum_failure_surfaces_instead_of_looping() {
    let server = StubHttpServer::start();
    let payload = file_bytes(50_000, 1);
    let mut model = model_with(&server, vec![("model.gguf", &payload)], false);
    // Manifest pins a DIFFERENT sha: every fetch verifies false.
    model.files[0].sha256 = "ab".repeat(32);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect_err("surfaces after the single automatic retry");
    assert!(matches!(err, DownloadError::ChecksumFailed { .. }));
    assert_eq!(
        server.requests().len(),
        2,
        "exactly one automatic retry, then surface (§5.2)"
    );
    assert!(!manager.is_installed("test-model"));
}

/// A quit that lands between the last byte and the verify leaves a
/// byte-COMPLETE part (founder dogfood, June 2026). A Range resume from
/// EOF draws 416 from real CDNs and the retry loop can never finish —
/// the fix verifies the part in place with ZERO network.
#[test]
fn byte_complete_part_verifies_and_installs_with_zero_network() {
    let server = StubHttpServer::start();
    let payload = file_bytes(80_000, 7);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("test-model")).unwrap();
    std::fs::write(dir.path().join("test-model/model.gguf.part"), &payload).unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("the complete part verifies in place");
    assert!(
        server.requests().is_empty(),
        "zero network: no resume request, no re-fetch"
    );
    assert_eq!(
        std::fs::read(dir.path().join("test-model/model.gguf")).unwrap(),
        payload
    );
    assert!(manager.is_installed("test-model"));
}

/// The evil twin: full length, wrong bytes. The in-place verify fails,
/// the part is deleted, and the single automatic retry fetches CLEAN
/// (no Range — the corrupt part must not survive into the resume).
#[test]
fn corrupt_byte_complete_part_restarts_clean_once() {
    let server = StubHttpServer::start();
    let payload = file_bytes(80_000, 7);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("test-model")).unwrap();
    std::fs::write(
        dir.path().join("test-model/model.gguf.part"),
        file_bytes(80_000, 201),
    )
    .unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("one clean re-fetch saves it");
    let requests = server.requests_for("/model.gguf");
    assert_eq!(
        requests.len(),
        1,
        "exactly one fetch, after the failed verify"
    );
    assert_eq!(requests[0].header("Range"), None, "clean, not a resume");
    assert_eq!(
        std::fs::read(dir.path().join("test-model/model.gguf")).unwrap(),
        payload
    );
}

/// §5.2: a model is installed only when ALL its files verify; files
/// download one at a time, in manifest order.
#[test]
fn installed_only_when_all_files_verify_one_file_at_a_time() {
    let server = StubHttpServer::start();
    let a = file_bytes(40_000, 11);
    let b = file_bytes(40_000, 12);
    let model = model_with(&server, vec![("a.gguf", &a), ("b.gguf", &b)], false);
    server.route("/a.gguf", StubResponse::RangedFile { file: a.clone() });
    // b: first attempt cut, then honest.
    let b_route = server.route(
        "/b.gguf",
        StubResponse::CutBody {
            status: 200,
            body: b.clone(),
            cut_after: 10_000,
            extra_headers: vec![],
        },
    );
    StubHttpServer::push(&b_route, StubResponse::RangedFile { file: b.clone() });

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect_err("file b cut");
    assert!(matches!(err, DownloadError::Interrupted { .. }));
    assert!(
        dir.path().join("test-model/a.gguf").exists(),
        "file a verified and renamed"
    );
    assert!(
        !manager.is_installed("test-model"),
        "NOT installed until all files verify"
    );
    // One at a time: every /a request precedes every /b request.
    let order: Vec<String> = server.requests().iter().map(|r| r.path.clone()).collect();
    let last_a = order.iter().rposition(|p| p.starts_with("/a")).unwrap();
    let first_b = order.iter().position(|p| p.starts_with("/b")).unwrap();
    assert!(last_a < first_b, "§5.2: one file at a time, got {order:?}");

    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("finishes b");
    assert!(manager.is_installed("test-model"));
    // The verified a.gguf was NOT re-fetched.
    assert_eq!(server.requests_for("/a.gguf").len(), 1);
}

/// §5.2: throttled while a capture session is live — the pacer seam is
/// consulted per chunk with the transferred sizes.
#[test]
fn pacer_seam_is_consulted_with_chunk_sizes() {
    struct Recording(Arc<Mutex<Vec<usize>>>);
    impl Pacer for Recording {
        fn pace(&mut self, just_transferred: usize) {
            self.0.lock().unwrap().push(just_transferred);
        }
    }
    let server = StubHttpServer::start();
    let payload = file_bytes(150_000, 4);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    manager
        .download_model(
            &model,
            1,
            &accepted(&model),
            &mut Recording(calls.clone()),
            &NO_CANCEL,
            NOW,
        )
        .expect("download");
    let calls = calls.lock().unwrap();
    assert!(!calls.is_empty(), "the throttle seam saw the transfer");
    assert_eq!(
        calls.iter().sum::<usize>(),
        payload.len(),
        "every byte passed through the pacer"
    );
}

#[test]
fn https_urls_are_refused_with_zero_traffic_until_the_p63_tls_client() {
    let model = ModelEntry {
        id: "hf-model".into(),
        role: "llm".into(),
        tiers: vec![1],
        license: License {
            name: "Apache-2.0".into(),
            url: "https://example.test".into(),
            acceptance_required: false,
        },
        total_bytes: 10,
        files: vec![FileEntry {
            repo: "hf:org/name".into(),
            revision: "abc".into(),
            path: "w.gguf".into(),
            sha256: "0".repeat(64),
            bytes: 10,
        }],
    };
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(
            &model,
            1,
            &Acceptances::default(),
            &mut NoPace,
            &NO_CANCEL,
            NOW,
        )
        .expect_err("an unpinned entry never reaches the network (B55)");
    assert!(matches!(err, DownloadError::Unpinned { .. }), "{err:?}");
}

#[test]
fn download_progress_rides_the_bus_coalesced() {
    let server = StubHttpServer::start();
    let payload = file_bytes(9_000_000, 2); // > 4 MB steps → ≥ 2 events
    let model = model_with(&server, vec![("big.gguf", &payload)], false);
    server.route(
        "/big.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let bus = RuntimeBus::new();
    let rx = bus.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), bus);
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("download");
    let mut progress = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let RuntimeEvent::DownloadProgress {
            downloaded_bytes,
            total_bytes,
            ..
        } = e
        {
            progress.push((downloaded_bytes, total_bytes));
        }
    }
    assert!(
        progress.len() >= 2,
        "coalesced progress events: {progress:?}"
    );
    assert!(
        progress.len() < 50,
        "…but COALESCED, not per-chunk: {}",
        progress.len()
    );
    assert!(
        progress.iter().all(|(_, t)| *t == model.total_bytes),
        "every event speaks in MODEL totals: {progress:?}"
    );
    assert_eq!(
        progress.last(),
        Some(&(payload.len() as u64, payload.len() as u64)),
        "completion event closes the series"
    );
}

/// Progress is MODEL-cumulative across files: the second file's events
/// carry the first file's bytes as a baseline, and every event's
/// denominator is the model's manifest total. The regression this pins:
/// per-file numerators displayed against the model total made DFN5B's
/// ~400-file enumeration read ~0% in settings while gigabytes sat
/// verified on disk.
#[test]
fn progress_is_model_cumulative_across_files() {
    let server = StubHttpServer::start();
    let a = file_bytes(5_000_000, 5); // each > the 4 MB coalescing step,
    let b = file_bytes(6_000_000, 6); // so both files emit in-flight events
    let model = model_with(&server, vec![("a.bin", &a), ("b.bin", &b)], false);
    server.route("/a.bin", StubResponse::RangedFile { file: a.clone() });
    server.route("/b.bin", StubResponse::RangedFile { file: b.clone() });
    let bus = RuntimeBus::new();
    let rx = bus.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), bus);
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("download");
    let mut progress = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let RuntimeEvent::DownloadProgress {
            downloaded_bytes,
            total_bytes,
            ..
        } = e
        {
            progress.push((downloaded_bytes, total_bytes));
        }
    }
    let total = (a.len() + b.len()) as u64;
    assert!(
        progress.iter().all(|(_, t)| *t == total),
        "every denominator is the MODEL total: {progress:?}"
    );
    assert!(
        progress.windows(2).all(|w| w[0].0 <= w[1].0),
        "the series never moves backwards (a per-file numerator would \
         reset to ~0 when file b starts): {progress:?}"
    );
    assert!(
        progress.iter().any(|(d, _)| *d > a.len() as u64),
        "file b's events ride on file a's completed bytes: {progress:?}"
    );
    assert_eq!(
        progress.last(),
        Some(&(total, total)),
        "the series closes at the model total"
    );
}

/// Path-preserving downloads: two files with the SAME basename in
/// DIFFERENT directories (DFN5B's visual/model.onnx and textual/model.onnx)
/// must NOT collide. Before the fix dest was dir.join(basename), so the
/// second `model.onnx` overwrote the first and the model could never verify
/// both files. Each must land at its own nested path with its own bytes.
#[test]
fn same_basename_in_different_dirs_do_not_collide() {
    let server = StubHttpServer::start();
    let visual = file_bytes(30_000, 21);
    let textual = file_bytes(20_000, 22);
    let model = model_with(
        &server,
        vec![
            ("visual/model.onnx", &visual),
            ("textual/model.onnx", &textual),
        ],
        false,
    );
    server.route(
        "/visual/model.onnx",
        StubResponse::RangedFile {
            file: visual.clone(),
        },
    );
    server.route(
        "/textual/model.onnx",
        StubResponse::RangedFile {
            file: textual.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("both nested files install");

    // Both files exist at their preserved relative paths with their OWN
    // bytes — no overwrite, no collision.
    let visual_dest = dir.path().join("test-model/visual/model.onnx");
    let textual_dest = dir.path().join("test-model/textual/model.onnx");
    assert_eq!(std::fs::read(&visual_dest).unwrap(), visual);
    assert_eq!(std::fs::read(&textual_dest).unwrap(), textual);
    assert!(manager.is_installed("test-model"));
    // Each was fetched exactly once — the collision would have forced a
    // re-fetch (or a verify failure) of the shared basename.
    assert_eq!(server.requests_for("/visual/model.onnx").len(), 1);
    assert_eq!(server.requests_for("/textual/model.onnx").len(), 1);
}

/// Containment pre-flight: a manifest entry whose path escapes the model
/// dir (a `..` traversal, here aimed at a sibling install) is rejected
/// BEFORE any directory is created or any byte moves. Path-preserving
/// downloads join file.path verbatim, so without this guard the bytes
/// would land outside models_dir/<id> — corrupting a sibling model that
/// remove_model/GC could never reclaim. Guards the L2-generated DFN5B
/// enumeration against a generator emitting paths relative to the wrong
/// root.
#[test]
fn path_escaping_the_model_dir_is_rejected_with_zero_side_effects() {
    let server = StubHttpServer::start();
    let payload = file_bytes(10_000, 33);
    let model = model_with(
        &server,
        vec![("../sibling-model/model.onnx", &payload)],
        false,
    );
    // The route exists; the point is that the pre-flight refuses before any
    // request is issued.
    server.route(
        "/../sibling-model/model.onnx",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect_err("the escaping path is refused");
    assert!(
        matches!(err, DownloadError::UnsafePath { ref file } if file == "../sibling-model/model.onnx"),
        "{err:?}"
    );

    // Nothing was created or written: not the model dir, not the sibling
    // target, and no socket was opened.
    assert!(!dir.path().join("test-model").exists());
    assert!(!dir.path().join("sibling-model").exists());
    assert!(!manager.is_installed("test-model"));
    assert!(
        server
            .requests_for("/../sibling-model/model.onnx")
            .is_empty()
    );
}

/// Resume across relaunch on a NESTED path: the cut, the part-file
/// location, the Range resume, and the atomic rename all operate at
/// dir/<file.path>, never the flattened basename.
#[test]
fn nested_path_resumes_from_the_byte_offset_across_relaunch() {
    let server = StubHttpServer::start();
    let payload = file_bytes(150_000, 33);
    let model = model_with(&server, vec![("visual/model.onnx", &payload)], false);
    let route = server.route(
        "/visual/model.onnx",
        StubResponse::CutBody {
            status: 200,
            body: payload.clone(),
            cut_after: 60_000,
            extra_headers: vec![],
        },
    );
    StubHttpServer::push(
        &route,
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );

    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect_err("cut interrupts the nested transfer");
    assert!(
        matches!(err, DownloadError::Interrupted { got_bytes: 60_000 }),
        "got {err:?}"
    );
    let part = dir.path().join("test-model/visual/model.onnx.part");
    assert_eq!(
        std::fs::metadata(&part).unwrap().len(),
        60_000,
        "the nested part file holds the progress"
    );

    // Relaunch: a fresh manager resumes from the nested part's length.
    let manager2 = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager2
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("resume completes at the nested path");
    let requests = server.requests_for("/visual/model.onnx");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("Range"), None, "first fetch from 0");
    assert_eq!(
        requests[1].header("Range"),
        Some("bytes=60000-"),
        "resume from the nested part-file length"
    );
    let dest = dir.path().join("test-model/visual/model.onnx");
    assert_eq!(std::fs::read(&dest).unwrap(), payload, "SHA-256 verified");
    assert!(
        !part.exists(),
        "atomic rename consumed the nested part file"
    );
    assert!(manager2.is_installed("test-model"));
}

/// Flat entries (path == basename, as the ASR/LLM manifest pins ship them)
/// land at dir/<basename> — IDENTICAL to the pre-path-preserving layout, so
/// nothing moves for already-installed models. This asserts the
/// invariant decision 1 promises: only nested entries gain subdirectories.
#[test]
fn flat_entry_layout_is_unchanged_for_installed_models() {
    let server = StubHttpServer::start();
    let payload = file_bytes(40_000, 44);
    // path == basename: exactly how encoder.int8.onnx / *.gguf are pinned.
    let model = model_with(&server, vec![("encoder.int8.onnx", &payload)], false);
    server.route(
        "/encoder.int8.onnx",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("flat entry installs");
    // Directly under models_dir/<id>/, NOT in any subdirectory.
    let dest = dir.path().join("test-model/encoder.int8.onnx");
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(manager.is_installed("test-model"));
}

/// D3: a cancel that lands before the transfer starts (queued model) stops
/// at the between-files check — zero requests, zero on-disk residue.
#[test]
fn cancel_before_the_first_byte_issues_no_request() {
    let server = StubHttpServer::start();
    let payload = file_bytes(50_000, 13);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let cancel = AtomicBool::new(true);
    let err = manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &cancel, NOW)
        .expect_err("pre-set cancel refuses before any byte moves");
    assert!(matches!(err, DownloadError::Cancelled), "{err:?}");
    assert!(server.requests().is_empty(), "no request was issued");
    assert!(!manager.is_installed("test-model"));
}

/// D3: a cancel observed mid-transfer (the per-chunk check next to the
/// pacer) stops promptly, KEEPS the part file, and a later download
/// resumes from its length — cancel is a pause, not a discard.
#[test]
fn cancel_mid_transfer_keeps_the_part_file_and_resumes_later() {
    /// Flips the shared cancel flag once the transfer passes a threshold —
    /// exactly how a settings-window cancel lands relative to the chunk
    /// loop (between two chunks).
    struct CancelAfter {
        cancel: Arc<AtomicBool>,
        seen: usize,
        after: usize,
    }
    impl Pacer for CancelAfter {
        fn pace(&mut self, just_transferred: usize) {
            self.seen += just_transferred;
            if self.seen >= self.after {
                self.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    let server = StubHttpServer::start();
    let payload = file_bytes(300_000, 17);
    let model = model_with(&server, vec![("model.gguf", &payload)], false);
    server.route(
        "/model.gguf",
        StubResponse::RangedFile {
            file: payload.clone(),
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let cancel = Arc::new(AtomicBool::new(false));
    let err = manager
        .download_model(
            &model,
            1,
            &accepted(&model),
            &mut CancelAfter {
                cancel: cancel.clone(),
                seen: 0,
                after: 64 * 1024,
            },
            &cancel,
            NOW,
        )
        .expect_err("cancel lands mid-transfer");
    assert!(matches!(err, DownloadError::Cancelled), "{err:?}");
    let part = dir.path().join("test-model/model.gguf.part");
    let kept = std::fs::metadata(&part).expect("part file kept").len();
    assert!(
        kept >= 64 * 1024 && kept < payload.len() as u64,
        "stopped promptly with the progress on disk: {kept}"
    );
    assert!(!manager.is_installed("test-model"));

    // The user clicks Download again: a FRESH flag resumes from the part.
    manager
        .download_model(&model, 1, &accepted(&model), &mut NoPace, &NO_CANCEL, NOW)
        .expect("resume completes");
    let requests = server.requests_for("/model.gguf");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].header("Range"),
        Some(format!("bytes={kept}-").as_str()),
        "the resume Ranges from the cancelled part's length"
    );
    assert!(manager.is_installed("test-model"));
}

/// REAL-NETWORK proof of the B66 transport (ignored: needs internet; run
/// manually — `cargo test --test runtime_download real_https -- --ignored`).
/// Fetches the smallest pinned artifact in the compiled manifest (the ASR
/// tokens.txt, ~9 KB) through the full manager path: https via ureq/rustls,
/// the HF resolve→CDN redirect, SHA-256 verification, atomic install.
#[test]
#[ignore]
fn real_https_fetch_verifies_the_smallest_pin() {
    use photoproof_core::runtime::manifest::compiled_manifest;
    let m = compiled_manifest();
    let asr = m
        .models
        .iter()
        .find(|x| x.role == "asr")
        .expect("asr entry");
    let tokens = asr
        .files
        .iter()
        .find(|f| f.path == "tokens.txt")
        .expect("tokens file")
        .clone();
    let tiny = photoproof_core::runtime::manifest::ModelEntry {
        id: "tokens-probe".into(),
        role: "test".into(),
        tiers: vec![1],
        license: photoproof_core::runtime::manifest::License {
            name: "n/a".into(),
            url: "n/a".into(),
            acceptance_required: false,
        },
        total_bytes: tokens.bytes,
        files: vec![tokens],
    };
    let dir = tempfile::tempdir().unwrap();
    let manager = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    let out = manager
        .download_model(
            &tiny,
            1,
            &Acceptances::default(),
            &mut NoPace,
            &NO_CANCEL,
            NOW,
        )
        .expect("real https fetch + verify");
    assert_eq!(out.files_fetched, 1);
    assert!(manager.is_installed("tokens-probe"));
}
