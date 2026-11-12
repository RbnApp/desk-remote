//! Free disk space of the receive folder.

use std::path::Path;

/// Free bytes available to the calling user on the volume holding `dir`; `None` when the
/// platform call fails (callers then do not block the transfer on a guess).
#[must_use]
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `free` is a valid out-pointer.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }
        .ok()
        .map(|()| free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_space_for_an_existing_folder() {
        let free = free_space(&std::env::temp_dir());
        assert!(free.is_some_and(|f| f > 0), "{free:?}");
    }
}
