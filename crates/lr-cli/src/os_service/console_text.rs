/// Text of a system tool's output: UTF-8 when it is valid, otherwise (on Windows) in the OEM
/// code page of the console, which `sc.exe` and its neighbours print in.
pub fn console_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => oem_text(bytes),
    }
}

#[cfg(windows)]
fn oem_text(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP};
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if len == 0 {
        return String::new();
    }
    let needed =
        unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    let written =
        unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

#[cfg(not(windows))]
fn oem_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::console_text;

    #[test]
    fn valid_utf8_passes_through() {
        assert_eq!(console_text("служба".as_bytes()), "служба");
        assert_eq!(console_text(b""), "");
    }

    #[cfg(windows)]
    #[test]
    fn bytes_that_are_not_utf8_are_read_in_the_oem_code_page() {
        let text = console_text(&[0x80, b'a', 0xFF]);
        assert!(text.contains('a') && !text.contains('\u{FFFD}'), "{text}");
        assert_eq!(text.chars().count(), 3);
    }
}
