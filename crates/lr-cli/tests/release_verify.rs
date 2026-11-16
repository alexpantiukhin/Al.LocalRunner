use lr_cli::release::{
    check_sha256, sums_entry, trusted_keys, verify_signature, VerifyError, NAMESPACE,
};

const KEY: &str = include_str!("fixtures/release/key.pub");
const KEY2: &str = include_str!("fixtures/release/key2.pub");
const SUMS: &str = include_str!("fixtures/release/SHA256SUMS");
const SIG: &str = include_str!("fixtures/release/SHA256SUMS.sig");
const BAD_NAMESPACE_SIG: &str = include_str!("fixtures/release/SHA256SUMS.bad.sig");
const OTHER_KEY_SIG: &str = include_str!("fixtures/release/SHA256SUMS.other.sig");

fn keys(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.trim().to_string()).collect()
}

#[test]
fn a_real_ssh_keygen_signature_verifies() {
    assert_eq!(
        verify_signature(&keys(&[KEY]), NAMESPACE, SUMS.as_bytes(), SIG),
        Ok(())
    );
}

#[test]
fn a_changed_message_fails() {
    let changed = SUMS.replacen("2cf24dba", "2cf24dbb", 1);
    assert_eq!(
        verify_signature(&keys(&[KEY]), NAMESPACE, changed.as_bytes(), SIG),
        Err(VerifyError::BadSignature)
    );
}

#[test]
fn another_namespace_fails() {
    assert_eq!(
        verify_signature(&keys(&[KEY]), NAMESPACE, SUMS.as_bytes(), BAD_NAMESPACE_SIG),
        Err(VerifyError::BadNamespace)
    );
    assert_eq!(
        verify_signature(&keys(&[KEY]), "git", SUMS.as_bytes(), SIG),
        Err(VerifyError::BadNamespace)
    );
}

#[test]
fn an_unknown_key_fails() {
    assert_eq!(
        verify_signature(&keys(&[KEY]), NAMESPACE, SUMS.as_bytes(), OTHER_KEY_SIG),
        Err(VerifyError::UnknownKey)
    );
}

#[test]
fn an_empty_key_list_is_no_keys() {
    assert_eq!(
        verify_signature(&[], NAMESPACE, SUMS.as_bytes(), SIG),
        Err(VerifyError::NoKeys)
    );
    assert_eq!(
        verify_signature(&keys(&["", "  "]), NAMESPACE, SUMS.as_bytes(), SIG),
        Err(VerifyError::NoKeys)
    );
}

#[test]
fn a_key_list_with_two_keys_accepts_either() {
    let both = keys(&[KEY2, KEY]);
    assert_eq!(
        verify_signature(&both, NAMESPACE, SUMS.as_bytes(), SIG),
        Ok(())
    );
    assert_eq!(
        verify_signature(&both, NAMESPACE, SUMS.as_bytes(), OTHER_KEY_SIG),
        Ok(())
    );
}

#[test]
fn garbage_armor_is_bad_armor() {
    let keys = keys(&[KEY]);
    for garbage in [
        "",
        "not a signature",
        "-----BEGIN SSH SIGNATURE-----\n!!!\n-----END SSH SIGNATURE-----",
        "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----",
        "-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----",
    ] {
        assert_eq!(
            verify_signature(&keys, NAMESPACE, SUMS.as_bytes(), garbage),
            Err(VerifyError::BadArmor),
            "{garbage:?}"
        );
    }
    let truncated = &SIG[..SIG.len() / 2];
    assert_eq!(
        verify_signature(&keys, NAMESPACE, SUMS.as_bytes(), truncated),
        Err(VerifyError::BadArmor)
    );
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn sums_entry_finds_a_file_and_rejects_a_missing_one() {
    let found = sums_entry(SUMS, "localrunner-linux-x86_64.tar.gz").unwrap();
    assert_eq!(
        hex(found),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert!(sums_entry(SUMS, "localrunner-windows-x86_64.zip").is_some());
    assert_eq!(sums_entry(SUMS, "localrunner-linux-x86_64"), None);
    assert_eq!(sums_entry(SUMS, "missing.zip"), None);
    assert_eq!(sums_entry("zz  file", "file"), None);
    let binary_mode = format!("{}  *file", hex([7; 32]));
    assert_eq!(sums_entry(&binary_mode, "file"), Some([7; 32]));
}

#[test]
fn check_sha256_detects_a_flipped_byte() {
    let expected = sums_entry(SUMS, "localrunner-linux-x86_64.tar.gz").unwrap();
    assert_eq!(check_sha256(b"hello", expected), Ok(()));
    assert_eq!(
        check_sha256(b"hellp", expected),
        Err(VerifyError::HashMismatch)
    );
}

#[test]
fn the_keys_of_this_build_have_no_blank_lines() {
    assert!(trusted_keys().iter().all(|k| !k.trim().is_empty()));
}
