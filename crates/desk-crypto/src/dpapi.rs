//! Windows Data Protection API (DPAPI) integration.
//!
//! Encrypts and decrypts sensitive key material at rest using Windows DPAPI.

use desk_error::{DeskError, DeskResult};

#[cfg(windows)]
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[cfg(windows)]
#[link(name = "crypt32")]
#[link(name = "kernel32")]
extern "system" {
    fn CryptProtectData(
        p_data_in: *const DataBlob,
        sz_data_descr: *const u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *mut std::ffi::c_void,
        p_prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;

    fn CryptUnprotectData(
        p_data_in: *const DataBlob,
        ppsz_data_descr: *mut *mut u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *mut std::ffi::c_void,
        p_prompt_struct: *mut std::ffi::c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;

    fn LocalFree(h_mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// Protects raw data using Windows DPAPI.
#[cfg(windows)]
pub fn protect(data: &[u8]) -> DeskResult<Vec<u8>> {
    let in_blob = DataBlob {
        cb_data: u32::try_from(data.len())
            .map_err(|_| DeskError::Crypto("Data size exceeds u32 limit".to_string()))?,
        pb_data: data.as_ptr().cast_mut(),
    };

    let mut out_blob = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };

    // CRYPTPROTECT_UI_FORBIDDEN = 0x1
    let success = unsafe {
        CryptProtectData(
            &in_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0x1,
            &mut out_blob,
        )
    };

    if success == 0 {
        return Err(DeskError::Crypto(format!(
            "CryptProtectData failed with Windows error: {}",
            std::io::Error::last_os_error()
        )));
    }

    let out_slice = unsafe {
        std::slice::from_raw_parts(
            out_blob.pb_data,
            usize::try_from(out_blob.cb_data).map_err(|_| {
                DeskError::Crypto("DPAPI output size conversion failed".to_string())
            })?,
        )
    };
    let result = out_slice.to_vec();

    unsafe {
        LocalFree(out_blob.pb_data.cast());
    }

    Ok(result)
}

/// Unprotects DPAPI-encrypted data on Windows.
#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> DeskResult<Vec<u8>> {
    let in_blob = DataBlob {
        cb_data: u32::try_from(data.len())
            .map_err(|_| DeskError::Crypto("Ciphertext exceeds u32 limit".to_string()))?,
        pb_data: data.as_ptr().cast_mut(),
    };

    let mut out_blob = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };

    // CRYPTPROTECT_UI_FORBIDDEN = 0x1
    let success = unsafe {
        CryptUnprotectData(
            &in_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0x1,
            &mut out_blob,
        )
    };

    if success == 0 {
        return Err(DeskError::Crypto(format!(
            "CryptUnprotectData failed with Windows error: {}",
            std::io::Error::last_os_error()
        )));
    }

    let out_slice = unsafe {
        std::slice::from_raw_parts(
            out_blob.pb_data,
            usize::try_from(out_blob.cb_data).map_err(|_| {
                DeskError::Crypto("DPAPI output size conversion failed".to_string())
            })?,
        )
    };
    let result = out_slice.to_vec();

    unsafe {
        LocalFree(out_blob.pb_data.cast());
    }

    Ok(result)
}

#[cfg(not(windows))]
pub fn protect(data: &[u8]) -> DeskResult<Vec<u8>> {
    // Non-Windows fallback for documentation / cross-compilation
    Ok(data.to_vec())
}

#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> DeskResult<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dpapi_roundtrip() -> DeskResult<()> {
        let secret = b"Desk-Remote super secret key material 1234567890";
        let encrypted = protect(secret)?;
        assert_ne!(encrypted, secret);
        let decrypted = unprotect(&encrypted)?;
        assert_eq!(decrypted, secret);
        Ok(())
    }
}
