//! Filesystem-level test of MasterKey::load_from_file — no container needed.
use spx_client::crypto::envelope::MasterKey;

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut n = [0u8; 8];
    getrandom::fill(&mut n).unwrap();
    let suffix = u64::from_le_bytes(n);
    std::env::temp_dir().join(format!("tower_master_key_{tag}_{suffix}"))
}

#[test]
fn loads_exactly_32_bytes() {
    let path = temp_path("ok");
    let key_bytes: [u8; 32] = [7u8; 32];
    std::fs::write(&path, key_bytes).unwrap();

    let mk = MasterKey::load_from_file(&path).expect("load 32-byte key");
    // Debug must be redacted (no key material).
    assert_eq!(format!("{mk:?}"), "MasterKey([REDACTED])");

    std::fs::remove_file(&path).ok();
}

#[test]
fn rejects_wrong_length() {
    let short = temp_path("short");
    std::fs::write(&short, [1u8; 16]).unwrap(); // 16 bytes, not 32
    assert!(MasterKey::load_from_file(&short).is_err(), "must reject a 16-byte file");
    std::fs::remove_file(&short).ok();

    let long = temp_path("long");
    std::fs::write(&long, [1u8; 64]).unwrap(); // 64 bytes
    assert!(MasterKey::load_from_file(&long).is_err(), "must reject a 64-byte file");
    std::fs::remove_file(&long).ok();
}

#[test]
fn missing_file_is_io_error() {
    assert!(MasterKey::load_from_file("/nonexistent/tower_master_key_xyz").is_err());
}
