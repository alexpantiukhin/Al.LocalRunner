use super::*;
use crate::ipc::{IpcAccess, IpcListener};
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;

const FILE_CREATE_PIPE_INSTANCE: u32 = 0x0004;
const USERS_SID: &str = "S-1-5-32-545";

fn own_sid() -> String {
    token_sid_string(&process_token().unwrap()).unwrap()
}

fn handle_sddl(handle: HANDLE) -> String {
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let what = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            what,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    assert_eq!(status, 0, "GetSecurityInfo");
    let _free = Descriptor(sd);
    let mut text: *mut u16 = null_mut();
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            what,
            &mut text,
            null_mut(),
        )
    };
    assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptor");
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe { LocalFree(text as *mut c_void) };
    s
}

async fn pipe_sddl_of(access: IpcAccess) -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let name = format!(
        r"\\.\pipe\lr-acl-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let _listener = IpcListener::bind(&name, access).await.unwrap();
    let client = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(&name)
        .unwrap();
    handle_sddl(client.as_raw_handle() as HANDLE)
}

#[test]
fn client_access_cannot_create_pipe_instances() {
    assert_eq!(PIPE_CLIENT_ACCESS & FILE_CREATE_PIPE_INSTANCE, 0);
}

#[test]
fn sddl_grants_owner_alone_or_listed_sids_with_client_rights() {
    assert_eq!(
        pipe_sddl("S-1-5-21-1", None),
        "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)"
    );
    let listed = pipe_sddl("S-1-5-21-1", Some(&["S-1-5-21-2".to_string()]));
    assert_eq!(
        listed,
        "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012018B;;;S-1-5-21-2)"
    );
    Descriptor::from_sddl(&listed).expect("valid SDDL");
}

#[tokio::test]
async fn owner_only_pipe_has_the_owner_alone() {
    let sddl = pipe_sddl_of(IpcAccess::OwnerOnly).await;
    let own = own_sid();
    assert!(sddl.starts_with(&format!("O:{own}D:P")), "{sddl}");
    let aces = sddl.matches("(A;").count();
    assert_eq!(aces, 1, "{sddl}");
    assert!(
        !sddl.contains(";;;WD)") && !sddl.contains(USERS_SID),
        "{sddl}"
    );
}

#[tokio::test]
async fn admin_channel_pipe_admits_owner_system_and_administrators_only() {
    let sddl = pipe_sddl_of(IpcAccess::OwnerAndAdmins).await;
    let own = own_sid();
    assert!(sddl.starts_with(&format!("O:{own}D:P")), "{sddl}");
    assert!(sddl.contains(";;;SY)") && sddl.contains(";;;BA)"), "{sddl}");
    assert_eq!(sddl.matches("(A;").count(), 3, "{sddl}");
    assert!(
        !sddl.contains(";;;WD)") && !sddl.contains(USERS_SID) && !sddl.contains(";;;BU)"),
        "{sddl}"
    );
}

#[tokio::test]
async fn listed_principal_gets_client_rights_only() {
    let sddl = pipe_sddl_of(IpcAccess::Principals(vec![USERS_SID.into()])).await;
    let own = own_sid();
    assert!(sddl.starts_with(&format!("O:{own}D:P")), "{sddl}");
    assert!(sddl.contains(";;;SY)") && sddl.contains(";;;BA)"), "{sddl}");
    assert!(
        sddl.contains("(A;;0x12018b;;;BU)") || sddl.contains("(A;;0x12018b;;;S-1-5-32-545)"),
        "{sddl}"
    );
    assert!(!sddl.contains(";;;WD)"), "{sddl}");
}

#[tokio::test]
async fn reload_changes_the_dacl_for_new_clients() {
    let name = format!(r"\\.\pipe\lr-acl-reload-{}", std::process::id());
    let listener = IpcListener::bind(&name, IpcAccess::OwnerOnly)
        .await
        .unwrap();
    let open = || {
        tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap()
    };
    let first = open();
    let before = handle_sddl(first.as_raw_handle() as HANDLE);
    assert!(
        !before.contains("BU)") && !before.contains(USERS_SID),
        "{before}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    listener
        .set_access(IpcAccess::Principals(vec![USERS_SID.into()]))
        .await
        .unwrap();
    let second = open();
    let after = handle_sddl(second.as_raw_handle() as HANDLE);
    assert!(
        after.contains("BU)") || after.contains(USERS_SID),
        "{after}"
    );
    listener.set_access(IpcAccess::OwnerOnly).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let third = open();
    let back = handle_sddl(third.as_raw_handle() as HANDLE);
    assert!(!back.contains("BU)") && !back.contains(USERS_SID), "{back}");
}

fn access(owner: Option<&str>, allowed: Option<&[&str]>) -> FileAccess {
    FileAccess {
        owner: owner.map(str::to_string),
        allowed: allowed.map(|a| a.iter().map(|s| s.to_string()).collect()),
    }
}

#[test]
fn private_file_needs_a_trusted_owner_and_trusted_entries() {
    let me = "S-1-5-21-7-7-7-1001";
    let other = "S-1-5-21-7-7-7-1002";
    assert!(access(Some(me), Some(&[me])).private_to(me));
    assert!(access(Some(SYSTEM_SID), Some(&[me, ADMINISTRATORS_SID])).private_to(me));
    assert!(access(Some(ADMINISTRATORS_SID), Some(&[me])).private_to(me));
    assert!(
        !access(Some(other), Some(&[me])).private_to(me),
        "another owner can rewrite the DACL"
    );
    assert!(!access(None, Some(&[me])).private_to(me));
    assert!(!access(Some(me), Some(&[me, other])).private_to(me));
    assert!(
        !access(Some(me), None).private_to(me),
        "no DACL is everyone"
    );
}

#[test]
fn own_key_file_reads_back_as_private() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../temp")
        .join(format!("lr-acl-own-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("k");
    crate::files::write_private(&path, b"x").unwrap();
    let got = file_access(&path).unwrap();
    assert_eq!(got.owner.as_deref(), Some(own_sid().as_str()));
    assert!(owner_only(&path).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}
