use crate::error::NetError;
use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, LookupAccountSidW, TokenUser, PSID, SID_NAME_USE,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// SID of the local SYSTEM account.
pub(crate) const SYSTEM_SID: &str = "S-1-5-18";
/// SID of the local Administrators group.
pub(crate) const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

pub(crate) fn last_error(what: &str) -> NetError {
    NetError::Io(format!("{what}: {}", std::io::Error::last_os_error()))
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Owned token handle, closed on drop.
pub(crate) struct Token(pub(crate) HANDLE);

unsafe impl Send for Token {}

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

/// `S-1-5-...` text of a SID.
pub(crate) fn sid_to_string(sid: PSID) -> Result<String, NetError> {
    let mut text: *mut u16 = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
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

/// Qualified account name `DOMAIN\name` of a SID; local accounts carry the computer name.
pub(crate) fn sid_account_name(sid: PSID) -> Result<String, NetError> {
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
    let (name, domain) = (from_wide(&name), from_wide(&domain));
    Ok(if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    })
}

/// Qualified account name of the SID given as text.
pub(crate) fn sid_text_account_name(sid: &str) -> Result<String, NetError> {
    let text = wide(sid);
    let mut psid: PSID = null_mut();
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut psid) } == 0 {
        return Err(last_error("ConvertStringSidToSid"));
    }
    let name = sid_account_name(psid);
    unsafe { LocalFree(psid) };
    name
}

/// Qualified account name of the user a token belongs to.
pub(crate) fn token_user_name(token: &Token) -> Result<String, NetError> {
    let buf = token_user(token)?;
    sid_account_name(user_sid(&buf))
}

/// String SID of the user a token belongs to.
pub(crate) fn token_sid_string(token: &Token) -> Result<String, NetError> {
    let buf = token_user(token)?;
    sid_to_string(user_sid(&buf))
}

/// Token of this process.
pub(crate) fn process_token() -> Result<Token, NetError> {
    let mut handle: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(last_error("OpenProcessToken"));
    }
    Ok(Token(handle))
}

/// NetBIOS name of this computer: the domain part of its local accounts.
pub(crate) fn computer_name() -> Result<String, NetError> {
    let mut buf = vec![0u16; 64];
    let mut len = buf.len() as u32;
    if unsafe { GetComputerNameExW(ComputerNameNetBIOS, buf.as_mut_ptr(), &mut len) } == 0 {
        return Err(last_error("GetComputerNameEx"));
    }
    Ok(from_wide(&buf))
}

/// An account name as `[[local_user]]` means it: `DOMAIN\name` stays, a bare `name` is a local
/// account of this computer.
pub(crate) fn qualify(name: &str, machine: &str) -> String {
    if name.contains('\\') {
        name.to_string()
    } else {
        format!("{machine}\\{name}")
    }
}

/// String SID of an account name, qualified with [`qualify`] first; a SID (`S-1-...`) is
/// checked and returned in its canonical text.
pub(crate) fn account_sid(name: &str) -> Result<String, NetError> {
    if name.starts_with("S-1-") || name.starts_with("s-1-") {
        let text = wide(name);
        let mut psid: PSID = null_mut();
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut psid) } == 0 {
            return Err(last_error(&format!("not a SID `{name}`")));
        }
        let canonical = sid_to_string(psid);
        unsafe { LocalFree(psid) };
        return canonical;
    }
    let qualified = wide(&qualify(name, &computer_name()?));
    let mut sid = vec![0u8; 256];
    let mut sid_len = sid.len() as u32;
    let mut domain = vec![0u16; 256];
    let mut domain_len = domain.len() as u32;
    let mut kind: SID_NAME_USE = 0;
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            qualified.as_ptr(),
            sid.as_mut_ptr() as PSID,
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        )
    };
    if ok == 0 {
        return Err(NetError::Io(format!(
            "unknown account `{name}`: {}",
            std::io::Error::last_os_error()
        )));
    }
    sid_to_string(sid.as_mut_ptr() as PSID)
}
