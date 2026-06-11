//! spec/RUNTIME.md §5 download manager against the scripted stub HTTP
//! server with cuttable connections. The §13 criteria covered here:
//! §13.4 (resume from the byte offset across relaunch; corrupted part
//! re-fetched without user action) and §13.7 (the license gate is
//! byte-zero — asserted AT THE SERVER, not the client).

mod common;

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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
        .expect_err("cut connection interrupts");
    assert!(
        matches!(err, DownloadError::Interrupted { got_bytes: 80_000 }),
        "got {err:?}"
    );
    let part = dir.path().join("test-model/model.gguf.part");
    assert_eq!(
        std::fs::metadata(&part).unwrap().len(),
        80_000,
        "the part file holds the progress"
    );
    assert!(!manager.is_installed("test-model"));

    // Relaunch: a NEW manager instance resumes from the part length.
    let manager2 = DownloadManager::new(dir.path().to_path_buf(), RuntimeBus::new());
    manager2
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
        .expect("resume completes");
    let requests = server.requests_for("/weights/model.gguf");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("Range"), None, "first fetch from 0");
    assert_eq!(
        requests[1].header("Range"),
        Some("bytes=80000-"),
        "§5.2: resume from the part-file length"
    );
    let dest = dir.path().join("test-model/model.gguf");
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
        .download_model(&model, 1, &Acceptances::default(), &mut NoPace, NOW)
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
        .expect_err("surfaces after the single automatic retry");
    assert!(matches!(err, DownloadError::ChecksumFailed { .. }));
    assert_eq!(
        server.requests().len(),
        2,
        "exactly one automatic retry, then surface (§5.2)"
    );
    assert!(!manager.is_installed("test-model"));
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
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
        .download_model(&model, 1, &Acceptances::default(), &mut NoPace, NOW)
        .expect_err("https needs the P6.3 TLS decision");
    assert!(matches!(err, DownloadError::TlsUnsupported { .. }));
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
        .download_model(&model, 1, &accepted(&model), &mut NoPace, NOW)
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
    assert_eq!(
        progress.last(),
        Some(&(payload.len() as u64, payload.len() as u64)),
        "completion event closes the series"
    );
}
