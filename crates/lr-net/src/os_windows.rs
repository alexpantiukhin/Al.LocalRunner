use crate::error::NetError;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, LookupAccountSidW, TokenUser, ACL,
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn last_error(what: &str) -> NetError {
    NetError::Io(format!("{what}: {}", std::io::Error::last_os_error()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Owned token handle, closed on drop.
pub(crate) struct Token(pub(crate) HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn token_user(token: &Token) -> Result<Vec<u8>, NetError> {
    let mut len = 0u32;
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(last_error("GetTokenInformation"));
    }
    let mut buf = vec![0u8; len as usize];
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr() as *mut c_void,
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(last_error("GetTokenInformation"));
    }
    Ok(buf)
}

fn user_sid(buf: &[u8]) -> PSID {
    let user = buf.as_ptr() as *const TOKEN_USER;
    unsafe { (*user).User.Sid }
}

/// Account name of the user a token belongs to, without the domain.
pub(crate) fn token_user_name(token: &Token) -> Result<String, NetError> {
    let buf = token_user(token)?;
    let sid = user_sid(&buf);
    let mut name = vec![0u16; 256];
    let mut domain = vec![0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind: SID_NAME_USE = 0;
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    if ok == 0 {
        return Err(last_error("LookupAccountSid"));
    }
    Ok(from_wide(&name))
}

/// String SID (`S-1-5-...`) of the user a token belongs to.
pub(crate) fn token_sid_string(token: &Token) -> Result<String, NetError> {
    let buf = token_user(token)?;
    let mut text: *mut u16 = null_mut();
    if unsafe { ConvertSidToStringSidW(user_sid(&buf), &mut text) } == 0 {
        return Err(last_error("ConvertSidToStringSid"));
    }
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    unsafe { LocalFree(text as *mut c_void) };
    Ok(s)
}

/// Token of this process.
pub(crate) fn process_token() -> Result<Token, NetError> {
    let mut handle: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    Ok(Token(handle))
}

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
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

/// Replaces the DACL of `path` with one that grants full access to the current user only and
/// inherits nothing from the parent directory.
pub(crate) fn restrict_to_owner(path: &Path) -> Result<(), NetError> {
    let sid = token_sid_string(&process_token()?)?;
    let sd = Descriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})"))?;
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
