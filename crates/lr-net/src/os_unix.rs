use crate::error::NetError;
use std::ffi::CStr;

/// Login name of `uid` from the user database; the number when the database has no entry.
pub(crate) fn user_name(uid: u32) -> String {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_name.is_null() {
        return uid.to_string();
    }
    unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_string_lossy()
        .into_owned()
}

/// Login name of the effective user of this process.
pub(crate) fn current_user() -> Result<String, NetError> {
    Ok(user_name(unsafe { libc::geteuid() }))
}
