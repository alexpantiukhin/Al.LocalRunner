use crate::acl_windows::Descriptor;
use crate::error::NetError;
use crate::os_windows::{
    account_sid, last_error, sid_to_string, wide, Token, ADMINISTRATORS_SID, SYSTEM_SID,
};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    NetApiBufferFree, NetUserGetLocalGroups, LG_INCLUDE_INDIRECT, LOCALGROUP_USERS_INFO_0,
    MAX_PREFERRED_LENGTH,
};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, ProgressInvokeNever, SetNamedSecurityInfoW, TreeResetNamedSecurityInfoW,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetAce, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    LookupPrivilegeValueW, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    LUID_AND_ATTRIBUTES, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const INHERITED_ACE: u8 = 0x10;

/// Who owns a file or directory and whom its own (not inherited) entries let in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSecurity {
    /// Owner SID; the owner can always rewrite the DACL.
    pub owner: Option<String>,
    /// SIDs of the explicit access-allowed entries; `None` when the DACL is missing (everyone)
    /// or holds an explicit entry of a kind other than plain allow or deny.
    pub explicit_allowed: Option<Vec<String>>,
    /// SIDs of all access-allowed entries, inherited ones included; `None` like
    /// `explicit_allowed`.
    pub allowed: Option<Vec<String>>,
}

/// The owner and the explicit allow entries of `path` (spec 10.3: the service state directory
/// must not stay reachable for whoever created it).
pub fn file_security(path: &Path) -> Result<FileSecurity, NetError> {
    let name = wide(&path.to_string_lossy());
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if status != 0 {
        return Err(NetError::Io(format!(
            "GetNamedSecurityInfo {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(status as i32)
        )));
    }
    let _free = Descriptor(sd);
    let owner = if owner.is_null() {
        None
    } else {
        Some(sid_to_string(owner)?)
    };
    if dacl.is_null() {
        return Ok(FileSecurity {
            owner,
            explicit_allowed: None,
            allowed: None,
        });
    }
    let mut allowed = Vec::new();
    let mut all = Vec::new();
    let count = unsafe { (*dacl).AceCount } as u32;
    for i in 0..count {
        let mut ace: *mut c_void = null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
            return Err(last_error("GetAce"));
        }
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        if header.AceFlags & INHERITED_ACE != 0 {
            if header.AceType == 0 {
                let entry = ace as *const ACCESS_ALLOWED_ACE;
                let sid = unsafe { &(*entry).SidStart } as *const u32 as PSID;
                all.push(sid_to_string(sid)?);
            }
            continue;
        }
        match header.AceType {
            0 => {
                let entry = ace as *const ACCESS_ALLOWED_ACE;
                let sid = unsafe { &(*entry).SidStart } as *const u32 as PSID;
                let sid = sid_to_string(sid)?;
                all.push(sid.clone());
                allowed.push(sid);
            }
            1 => {}
            _ => {
                return Ok(FileSecurity {
                    owner,
                    explicit_allowed: None,
                    allowed: None,
                })
            }
        }
    }
    Ok(FileSecurity {
        owner,
        explicit_allowed: Some(allowed),
        allowed: Some(all),
    })
}

/// Closes directory `path` to user `user_sid`, Administrators and SYSTEM: a protected DACL
/// that inherits nothing from the parent; the owner stays. Needs no privilege when this
/// process's user owns `path`; fails otherwise.
pub fn close_dir_to_owner(path: &Path, user_sid: &str) -> Result<(), NetError> {
    let sd = Descriptor::from_sddl(&format!(
        "D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;{ADMINISTRATORS_SID})(A;OICI;FA;;;{SYSTEM_SID})"
    ))?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(last_error("GetSecurityDescriptorDacl"));
    }
    let name = wide(&path.to_string_lossy());
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(NetError::Io(format!(
            "SetNamedSecurityInfo {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(status as i32)
        )));
    }
    Ok(())
}

/// SDDL of the broker service's state directory: owned by the service account, which has full
/// access together with Administrators and SYSTEM; nothing inherited from the parent.
pub fn service_dir_sddl(service_sid: &str) -> String {
    format!(
        "O:{service_sid}D:P(A;OICI;FA;;;{service_sid})(A;OICI;FA;;;{ADMINISTRATORS_SID})(A;OICI;FA;;;{SYSTEM_SID})"
    )
}

fn enable_privilege(name: &str) -> Result<(), NetError> {
    let mut handle: HANDLE = null_mut();
    let rights = TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY;
    if unsafe { OpenProcessToken(GetCurrentProcess(), rights, &mut handle) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    let token = Token(handle);
    let text = wide(name);
    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: unsafe { std::mem::zeroed() },
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    if unsafe {
        LookupPrivilegeValueW(
            std::ptr::null(),
            text.as_ptr(),
            &mut privileges.Privileges[0].Luid,
        )
    } == 0
    {
        return Err(last_error("LookupPrivilegeValue"));
    }
    let ok = unsafe { AdjustTokenPrivileges(token.0, 0, &privileges, 0, null_mut(), null_mut()) };
    let error = std::io::Error::last_os_error();
    if ok == 0 || error.raw_os_error() == Some(1300) {
        return Err(NetError::Io(format!("cannot enable {name}: {error}")));
    }
    Ok(())
}

/// Gives `path` the security of [`service_dir_sddl`]: the protected DACL and the service
/// account as owner. With `whole_tree`, every file and directory below also loses its explicit
/// entries, inherits that DACL and gets the same owner; without it, those keep their owners and
/// explicit entries (so they can still be checked). Needs an elevated process.
pub fn lock_service_dir(path: &Path, service_sid: &str, whole_tree: bool) -> Result<(), NetError> {
    enable_privilege("SeRestorePrivilege")?;
    let sd = Descriptor::from_sddl(&service_dir_sddl(service_sid))?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(last_error("GetSecurityDescriptorDacl"));
    }
    let mut owner: PSID = null_mut();
    if unsafe { GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted) } == 0 {
        return Err(last_error("GetSecurityDescriptorOwner"));
    }
    let name = wide(&path.to_string_lossy());
    let what = OWNER_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION;
    if !whole_tree {
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                what,
                owner,
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        if status != 0 {
            return Err(NetError::Io(format!(
                "SetNamedSecurityInfo {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(status as i32)
            )));
        }
        return Ok(());
    }
    let status = unsafe {
        TreeResetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            null_mut(),
            dacl,
            null_mut(),
            0,
            None,
            ProgressInvokeNever,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(NetError::Io(format!(
            "TreeResetNamedSecurityInfo {}: {}",
            path.display(),
            std::io::Error::from_raw_os_error(status as i32)
        )));
    }
    Ok(())
}

/// Creates directory `path` (its parent must exist) already carrying the security of
/// [`service_dir_sddl`], so it is never open to anyone else, not even for a moment. Fails when
/// it exists.
pub fn create_service_dir(path: &Path, service_sid: &str) -> Result<(), NetError> {
    enable_privilege("SeRestorePrivilege")?;
    let sd = Descriptor::from_sddl(&service_dir_sddl(service_sid))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let name = wide(&path.to_string_lossy());
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(NetError::Io(format!(
            "CreateDirectory {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Number of hard links of the file at `path` (a link itself is not followed).
pub fn hard_links(path: &Path) -> Result<u32, NetError> {
    let name = wide(&path.to_string_lossy());
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(NetError::Io(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    let handle = Token(handle);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
        return Err(last_error("GetFileInformationByHandle"));
    }
    Ok(info.nNumberOfLinks)
}

fn local_groups(user: &str) -> Vec<String> {
    let name = wide(user);
    let mut buf: *mut u8 = null_mut();
    let (mut read, mut total) = (0u32, 0u32);
    let status = unsafe {
        NetUserGetLocalGroups(
            std::ptr::null(),
            name.as_ptr(),
            0,
            LG_INCLUDE_INDIRECT,
            &mut buf,
            MAX_PREFERRED_LENGTH,
            &mut read,
            &mut total,
        )
    };
    if status != 0 || buf.is_null() {
        return Vec::new();
    }
    let entries =
        unsafe { std::slice::from_raw_parts(buf as *const LOCALGROUP_USERS_INFO_0, read as usize) };
    let mut out = Vec::new();
    for e in entries {
        let mut len = 0;
        while unsafe { *e.lgrui0_name.add(len) } != 0 {
            len += 1;
        }
        out.push(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(e.lgrui0_name, len)
        }));
    }
    unsafe { NetApiBufferFree(buf as *const c_void) };
    out
}

/// Whether the account is an administrator of this machine: SYSTEM or a member (directly or
/// through a group) of the local Administrators group, elevated or not.
pub(crate) fn is_machine_admin(os_user: &str) -> bool {
    match account_sid(os_user) {
        Ok(sid) if sid == SYSTEM_SID || sid == ADMINISTRATORS_SID => return true,
        Err(_) => return false,
        Ok(_) => {}
    }
    let Ok(admins) = crate::os_windows::sid_text_account_name(ADMINISTRATORS_SID) else {
        return false;
    };
    let group = admins.rsplit('\\').next().unwrap_or(&admins).to_string();
    local_groups(os_user)
        .iter()
        .any(|g| g.eq_ignore_ascii_case(&group))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os_windows::{process_token, token_sid_string};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lr-svc-acl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_new_directory_has_only_inherited_entries_and_an_explicit_grant_shows() {
        let dir = temp_dir("explicit");
        let own = token_sid_string(&process_token().unwrap()).unwrap();
        let fresh = file_security(&dir).unwrap();
        assert_eq!(fresh.owner.as_deref(), Some(own.as_str()));
        assert_eq!(fresh.explicit_allowed, Some(Vec::new()));
        crate::acl_windows::restrict_to_owner(&dir).unwrap();
        assert_eq!(
            file_security(&dir).unwrap().explicit_allowed,
            Some(vec![own])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_second_hard_link_is_counted() {
        let dir = temp_dir("links");
        let file = dir.join("a");
        std::fs::write(&file, "").unwrap();
        assert_eq!(hard_links(&file).unwrap(), 1);
        std::fs::hard_link(&file, dir.join("b")).unwrap();
        assert_eq!(hard_links(&file).unwrap(), 2);
        assert_eq!(hard_links(&dir).unwrap(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn service_dir_sddl_is_owned_by_the_service_and_admits_admins_and_system() {
        let sddl = service_dir_sddl("S-1-5-80-1");
        assert_eq!(
            sddl,
            "O:S-1-5-80-1D:P(A;OICI;FA;;;S-1-5-80-1)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;FA;;;S-1-5-18)"
        );
        Descriptor::from_sddl(&sddl).expect("valid SDDL");
    }

    #[test]
    fn system_administers_the_machine_and_an_unknown_account_does_not() {
        assert!(is_machine_admin(r"NT AUTHORITY\SYSTEM"));
        assert!(!is_machine_admin(r"NT AUTHORITY\NETWORK SERVICE"));
        assert!(!is_machine_admin("no-such-user-lr-test"));
    }
}
