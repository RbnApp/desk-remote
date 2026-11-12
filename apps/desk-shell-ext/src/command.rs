//! COM plumbing: the `IExplorerCommand` implementation and the DLL entry points.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{implement, Error, Interface, Ref, Result, BOOL, GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOTIMPL, E_POINTER, HMODULE,
    S_FALSE, S_OK, TRUE,
};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW,
    ECF_DEFAULT, ECS_ENABLED, SIGDN_FILESYSPATH,
};

/// Class id declared in `packaging/AppxManifest.xml.in` (windows.comServer + the verb).
pub const CLSID_DESK_REMOTE_SEND: GUID = GUID::from_u128(0x7c1f5b2e_3d4a_4e8b_9a61_5d0e2f8b4c13);

/// Handle of this DLL, stored at load time to find the panel installed beside it.
static MODULE: AtomicIsize = AtomicIsize::new(0);

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0 as isize, Ordering::SeqCst);
    }
    TRUE
}

fn module_dir() -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let handle = HMODULE(MODULE.load(Ordering::SeqCst) as *mut c_void);
    // SAFETY: `buf` is a valid writable slice; the handle is this DLL's own module.
    let len = unsafe { GetModuleFileNameW(Some(handle), &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    let dll = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    dll.parent().map(PathBuf::from)
}

fn panel_exe() -> Option<PathBuf> {
    module_dir().map(|d| d.join(desk_paths::PANEL_EXE_NAME))
}

fn locale_name() -> String {
    let mut buf = [0u16; 85];
    // SAFETY: `buf` is a valid writable slice of the documented maximum size.
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(len as usize) - 1])
}

fn dup(text: &str) -> Result<PWSTR> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; the shell frees the copy.
    unsafe { SHStrDupW(windows::core::PCWSTR(wide.as_ptr())) }
}

/// File-system paths of the selected items (non-file items are skipped).
fn selected_paths(items: Option<&IShellItemArray>) -> Vec<String> {
    let Some(items) = items else {
        return Vec::new();
    };
    // SAFETY: plain COM calls on a live IShellItemArray handed over by Explorer.
    unsafe {
        let count = items.GetCount().unwrap_or(0);
        (0..count)
            .filter_map(|i| {
                let item = items.GetItemAt(i).ok()?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let text = name.to_string().ok();
                CoTaskMemFree(Some(name.0 as *const c_void));
                text
            })
            .collect()
    }
}

#[implement(IExplorerCommand)]
struct SendCommand;

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for SendCommand_Impl {
    fn GetTitle(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        dup(&crate::menu_title(&locale_name()))
    }

    fn GetIcon(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        let exe = panel_exe().ok_or_else(|| Error::from(E_FAIL))?;
        dup(&format!("{},0", exe.display()))
    }

    fn GetToolTip(&self, _items: Ref<'_, IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID_DESK_REMOTE_SEND)
    }

    fn GetState(&self, _items: Ref<'_, IShellItemArray>, _slow: BOOL) -> Result<u32> {
        Ok(ECS_ENABLED.0 as u32)
    }

    fn Invoke(&self, items: Ref<'_, IShellItemArray>, _bind: Ref<'_, IBindCtx>) -> Result<()> {
        let paths = selected_paths(items.as_ref());
        if paths.is_empty() {
            return Ok(());
        }
        let exe = panel_exe().ok_or_else(|| Error::from(E_FAIL))?;
        desk_paths::hidden_command(exe)
            .args(crate::panel_args(&paths))
            .spawn()
            .map(|_| ())
            .map_err(|_| Error::from(E_FAIL))
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory)]
struct Factory;

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, windows::core::IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let command: IExplorerCommand = SendCommand.into();
        // SAFETY: both pointers were checked above; `query` writes an owned interface pointer.
        unsafe { command.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// COM entry point: hands out the class factory for [`CLSID_DESK_REMOTE_SEND`].
///
/// # Safety
/// Called by COM with valid (or null) pointers; they are checked before use.
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || iid.is_null() || object.is_null() {
        return E_POINTER;
    }
    // SAFETY: pointers checked for null above.
    unsafe {
        *object = std::ptr::null_mut();
        if *clsid != CLSID_DESK_REMOTE_SEND {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(iid, object)
    }
}

/// The class is stateless; the host may always unload the DLL once all references are gone.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if MODULE.load(Ordering::SeqCst) == 0 {
        S_FALSE
    } else {
        S_OK
    }
}
