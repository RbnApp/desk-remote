//! Loads the built DLL like Explorer does (LoadLibrary + DllGetClassObject), asks the verb for
//! its title and icon, invokes it on two real files and checks that the panel stand-in (this very
//! test executable, copied as `desk-panel.exe` beside the DLL) receives `--send <paths>`.
//! `harness = false`: the same `main` is the stand-in when started with `--send`.
// A test binary that reports through stdout and fails by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

#[cfg(windows)]
fn main() {
    use windows::core::{Interface, GUID, HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoInitializeEx, IClassFactory, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Shell::{
        IExplorerCommand, IShellItem, IShellItemArray, SHCreateItemFromParsingName,
        SHCreateShellItemArrayFromShellItem,
    };

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--send") {
        // Stand-in for desk-panel.exe: record what the menu handed over.
        if let Ok(out) = std::env::var("DESK_STUB_OUT") {
            let _ = std::fs::write(out, args[2..].join("\n"));
        }
        return;
    }

    let exe = std::env::current_exe().expect("exe");
    let target_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target dir")
        .to_path_buf();
    let dll_src = target_dir.join("desk_remote_menu.dll");
    if !dll_src.is_file() {
        // `cargo test` does not link the package's own cdylib; build it (the test run holds no cargo lock).
        let mut build =
            std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        build.args(["build", "-p", "desk-shell-ext"]);
        if target_dir.ends_with("release") {
            build.arg("--release");
        }
        assert!(
            build.status().expect("cargo").success(),
            "cargo build failed"
        );
    }
    assert!(dll_src.is_file(), "missing {}", dll_src.display());
    let work = std::env::temp_dir().join(format!("desk-menu-com-{}", std::process::id()));
    std::fs::create_dir_all(&work).expect("work dir");
    let dll = work.join("desk_remote_menu.dll");
    std::fs::copy(&dll_src, &dll).expect("copy dll");
    std::fs::copy(&exe, work.join("desk-panel.exe")).expect("copy stub");
    let file_a = work.join("a b.txt");
    std::fs::write(&file_a, b"a").expect("file a");
    let out = work.join("received.txt");
    std::env::set_var("DESK_STUB_OUT", &out);

    // SAFETY: plain COM/loader calls on objects created here.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let module = LoadLibraryW(&HSTRING::from(dll.as_os_str())).expect("LoadLibrary");
        let proc = GetProcAddress(module, windows::core::s!("DllGetClassObject")).expect("export");
        type GetClassObject = unsafe extern "system" fn(
            *const GUID,
            *const GUID,
            *mut *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        let get: GetClassObject = std::mem::transmute(proc);
        let clsid = GUID::from_u128(0x7c1f5b2e_3d4a_4e8b_9a61_5d0e2f8b4c13);
        let mut raw = std::ptr::null_mut();
        get(&clsid, &IClassFactory::IID, &mut raw)
            .ok()
            .expect("DllGetClassObject");
        let factory = IClassFactory::from_raw(raw);
        let command: IExplorerCommand = factory.CreateInstance(None).expect("CreateInstance");

        let title = command.GetTitle(None::<&IShellItemArray>).expect("title");
        println!("title = {}", title.to_string().expect("utf16"));
        let icon = command.GetIcon(None::<&IShellItemArray>).expect("icon");
        let icon = icon.to_string().expect("utf16");
        println!("icon  = {icon}");
        assert!(icon.ends_with("desk-panel.exe,0"), "{icon}");
        assert_eq!(command.GetCanonicalName().expect("name"), clsid);

        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(HSTRING::from(file_a.as_os_str()).as_ptr()), None)
                .expect("item a");
        let array =
            SHCreateShellItemArrayFromShellItem::<_, IShellItemArray>(&item).expect("array");
        command.Invoke(&array, None).expect("Invoke");
    }

    for _ in 0..50 {
        if out.is_file() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let received = std::fs::read_to_string(&out).expect("stand-in received nothing");
    println!("panel got = {received:?}");
    assert_eq!(received, file_a.display().to_string());
    let _ = std::fs::remove_dir_all(&work);
    println!("COM roundtrip OK");
}

#[cfg(not(windows))]
fn main() {}
