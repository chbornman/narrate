use photoproof_core::runtime::{ControlFileSource, save_control};
use photoproof_core::tuning::{Tuning, replace, tuning};

#[test]
fn validated_tuning_replacement_is_atomic_and_invalid_bytes_keep_last_known_good() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tuning.toml");
    save_control(&path, b"[search]\nrrf_k = 41.0\n").unwrap();
    let first = Tuning::load_checked(dir.path()).unwrap();
    replace(first.value);
    assert_eq!(tuning().search.rrf_k, 41.0);

    std::fs::write(&path, b"[search\nrrf_k =").unwrap();
    let recovered = Tuning::load_checked(dir.path()).unwrap();
    assert_eq!(recovered.recovery.source, ControlFileSource::LastKnownGood);
    assert_eq!(
        tuning().search.rrf_k,
        41.0,
        "loading/recovery alone cannot expose a partial candidate"
    );
    replace(recovered.value);
    assert_eq!(tuning().search.rrf_k, 41.0);

    save_control(&path, b"[search]\nrrf_k = 73.0\n").unwrap();
    let second = Tuning::load_checked(dir.path()).unwrap();
    replace(second.value);
    assert_eq!(tuning().search.rrf_k, 73.0);
}
