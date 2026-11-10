use crate::error::NetError;
use crate::os_windows::{
    last_error, process_token, sid_to_string, token_sid_string, wide, ADMINISTRATORS_SID,
    SYSTEM_SID,
};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, GetSecurityInfo,
    SetNamedSecurityInfoW, SetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetAce, GetSecurityDescriptorDacl, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID,
};

/// Access a pipe client gets: read, write data and attributes, read control; never
/// `FILE_CREATE_PIPE_INSTANCE`, so a client cannot add server instances of the pipe.
pub(crate) const PIPE_CLIENT_ACCESS: u32 = 0x0012_018B;

/// A security descriptor built from SDDL, freed on drop.
pub(crate) struct Descriptor(pub(crate) PSECURITY_DESCRIPTOR);

impl Descriptor {
    pub(crate) fn from_sddl(sddl: &str) -> Result<Descriptor, NetError> {
        let text = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error("ConvertStringSecurityDescriptor"));
        }
        Ok(Descriptor(sd))
    }

    fn dacl(&self) -> Result<*mut ACL, NetError> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = null_mut();
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
        {
            return Err(last_error("GetSecurityDescriptorDacl"));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

/// SDDL of a named pipe owned by `owner`. `None`: the owner alone. `Some(sids)`: the owner,
/// SYSTEM and Administrators in full, and each of `sids` with [`PIPE_CLIENT_ACCESS`] only.
/// The DACL is protected (nothing inherited); the owner is set explicitly, so a client can
/// check who created the pipe.
pub(crate) fn pipe_sddl(owner: &str, admitted: Option<&[String]>) -> String {
    let mut sddl = format!("O:{owner}D:P(A;;GA;;;{owner})");
    if let Some(sids) = admitted {
        sddl.push_str("(A;;GA;;;SY)(A;;GA;;;BA)");
        for sid in sids {
            sddl.push_str(&format!("(A;;0x{PIPE_CLIENT_ACCESS:08X};;;{sid})"));
        }
    }
    sddl
}

/// Replaces the DACL of `path` with one that grants full access to the current user only and
/// inherits nothing from the parent directory.
pub(crate) fn restrict_to_owner(path: &Path) -> Result<(), NetError> {
    let sid = token_sid_string(&process_token()?)?;
    let sd = Descriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})"))?;
    let dacl = sd.dacl()?;
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

/// Replaces the DACL of a kernel object (a named pipe: the DACL is shared by all instances of
/// the pipe name) with the one of `sddl`; handles opened before keep their access.
pub(crate) fn set_handle_dacl(handle: HANDLE, sddl: &str) -> Result<(), NetError> {
    let sd = Descriptor::from_sddl(sddl)?;
    let dacl = sd.dacl()?;
    let status = unsafe {
        SetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(NetError::Io(format!(
            "SetSecurityInfo: {}",
            std::io::Error::from_raw_os_error(status as i32)
        )));
    }
    Ok(())
}

/// Whether every access-allowed entry of the DACL of `path` names the current user, SYSTEM or
/// Administrators. A missing DACL (everyone) or any other allowed SID makes the file insecure.
pub(crate) fn owner_only(path: &Path) -> Result<bool, NetError> {
    let own = token_sid_string(&process_token()?)?;
    let name = wide(&path.to_string_lossy());
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
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
    if dacl.is_null() {
        return Ok(false);
    }
    let count = unsafe { (*dacl).AceCount } as u32;
    for i in 0..count {
        let mut ace: *mut c_void = null_mut();
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 {
            return Err(last_error("GetAce"));
        }
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        match header.AceType {
            0 => {
                let allowed = ace as *const ACCESS_ALLOWED_ACE;
                let sid = unsafe { &(*allowed).SidStart } as *const u32 as PSID;
                let sid = sid_to_string(sid)?;
                let trusted = [own.as_str(), SYSTEM_SID, ADMINISTRATORS_SID];
                if !trusted.iter().any(|t| t.eq_ignore_ascii_case(&sid)) {
                    return Ok(false);
                }
            }
            1 => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// String SID of the owner of a kernel object handle (a pipe).
pub(crate) fn handle_owner(handle: HANDLE) -> Result<String, NetError> {
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if status != 0 {
        return Err(NetError::Io(format!(
            "GetSecurityInfo: {}",
            std::io::Error::from_raw_os_error(status as i32)
        )));
    }
    let _free = Descriptor(sd);
    if owner.is_null() {
        return Err(NetError::Io("the pipe has no owner".into()));
    }
    sid_to_string(owner)
}

#[cfg(test)]
#[path = "acl_windows_tests.rs"]
mod tests;
