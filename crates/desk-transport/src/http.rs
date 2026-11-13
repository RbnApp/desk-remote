//! # Desk-Remote Transport - HTTP(S) GET client
//!
//! ## Responsibility
//! The only HTTP client of the code base: a blocking GET over the Windows system stack
//! (WinHTTP: system proxy, system certificate store, TLS), either into memory with a size
//! cap or straight into a file with progress. Used by `desk-update` to read a release and
//! download an installer.
//!
//! ## Scope Boundaries
//! - GET only, no cookies, no credentials, no request body.
//! - Knows nothing about releases, hashes or versions: that is `desk-update`.
//! - Only `http://` and `https://`; redirects are followed by WinHTTP (never HTTPS -> HTTP).
//!
//! ## Dependencies
//! `desk-error`; the `windows` crate (WinHTTP).

use desk_error::{DeskError, DeskResult};
use std::ffi::c_void;
use std::io::Write;
use std::path::Path;
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS,
};

/// Sent as `User-Agent` (GitHub rejects requests without one). Carries no machine data.
const USER_AGENT: PCWSTR = w!("Desk-Remote-Updater");
/// `WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER`.
const QUERY_STATUS_NUMBER: u32 = 19 | 0x2000_0000;
/// `WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER`.
const QUERY_LENGTH_NUMBER: u32 = 5 | 0x2000_0000;
/// `WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE`.
const ADD_HEADER_REPLACE: u32 = 0x2000_0000 | 0x8000_0000;
/// Resolve / connect / send / receive timeouts in milliseconds.
const TIMEOUTS_MS: [i32; 4] = [10_000, 10_000, 30_000, 30_000];
const CHUNK: usize = 64 * 1024;

fn http_err(what: &str, e: impl std::fmt::Display) -> DeskError {
    DeskError::Transport(format!("http {what}: {e}"))
}

/// A WinHTTP handle closed on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> DeskResult<Self> {
        if raw.is_null() {
            Err(http_err(what, windows::core::Error::from_thread()))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by WinHTTP and is closed exactly once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// `scheme://host[:port]/path?query` split into what WinHTTP needs.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_target(url: &str) -> DeskResult<Target> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(DeskError::Transport(
            "http: only http:// and https:// URLs are allowed".to_string(),
        ));
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || authority.contains('@') {
        return Err(DeskError::Transport("http: invalid host".to_string()));
    }
    let default_port = if secure { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => (
            h.trim_matches(['[', ']']).to_string(),
            p.parse::<u16>()
                .map_err(|_| DeskError::Transport("http: invalid port".to_string()))?,
        ),
        _ => (authority.trim_matches(['[', ']']).to_string(), default_port),
    };
    if host.is_empty() {
        return Err(DeskError::Transport("http: invalid host".to_string()));
    }
    Ok(Target {
        secure,
        host,
        port,
        path,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Query a numeric header or status; `None` when absent.
fn query_number(request: &Handle, level: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` and `size` are valid for the whole call and `size` matches `value`.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            level,
            PCWSTR::null(),
            Some((&mut value as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
    }
    .ok()
    .map(|()| value)
}

/// Runs the GET and feeds the body to `on_chunk`. `on_length` gets the announced size
/// (when the server sent one) before the first byte. Fails when the status is not 2xx or
/// the body is larger than `max_bytes`.
fn stream(
    url: &str,
    max_bytes: u64,
    on_length: &mut dyn FnMut(Option<u64>) -> DeskResult<()>,
    on_chunk: &mut dyn FnMut(&[u8]) -> DeskResult<()>,
) -> DeskResult<()> {
    let target = parse_target(url)?;
    // SAFETY: every pointer handed to WinHTTP below outlives the call it is passed to; the
    // handles are closed by `Handle::drop` in reverse order of creation.
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                USER_AGENT,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "open",
        )?;
        let [resolve, connect, send, receive] = TIMEOUTS_MS;
        WinHttpSetTimeouts(session.0, resolve, connect, send, receive)
            .map_err(|e| http_err("timeouts", e))?;
        let host = wide(&target.host);
        let connection = Handle::new(
            WinHttpConnect(session.0, PCWSTR(host.as_ptr()), target.port, 0),
            "connect",
        )?;
        let path = wide(&target.path);
        let flags = if target.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "request",
        )?;
        let accept: Vec<u16> = "Accept: application/octet-stream, application/vnd.github+json\r\n"
            .encode_utf16()
            .collect();
        let _ = WinHttpAddRequestHeaders(request.0, &accept, ADD_HEADER_REPLACE);
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| http_err("send", e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| http_err("receive", e))?;

        let status = query_number(&request, QUERY_STATUS_NUMBER).unwrap_or(0);
        if !(200..300).contains(&status) {
            return Err(DeskError::Transport(format!("http status {status}")));
        }
        let announced = query_number(&request, QUERY_LENGTH_NUMBER).map(u64::from);
        if announced.is_some_and(|n| n > max_bytes) {
            return Err(DeskError::Transport(
                "http: response is larger than allowed".to_string(),
            ));
        }
        on_length(announced)?;
        let mut buffer = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                CHUNK as u32,
                &mut read,
            )
            .map_err(|e| http_err("read", e))?;
            if read == 0 {
                return Ok(());
            }
            total += u64::from(read);
            if total > max_bytes {
                return Err(DeskError::Transport(
                    "http: response is larger than allowed".to_string(),
                ));
            }
            on_chunk(&buffer[..read as usize])?;
        }
    }
}

/// GET `url` into memory; fails past `max_bytes`.
pub fn get_bytes(url: &str, max_bytes: u64) -> DeskResult<Vec<u8>> {
    let mut body = Vec::new();
    stream(url, max_bytes, &mut |_| Ok(()), &mut |chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(body)
}

/// GET `url` into the file `dest` (created or truncated). `progress(received, total)` runs
/// after every chunk; a partly written file is removed on failure. Returns the byte count.
pub fn download(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> DeskResult<u64> {
    let mut file = std::fs::File::create(dest)?;
    let total_len = std::cell::Cell::new(None);
    let mut received = 0u64;
    let result = stream(
        url,
        max_bytes,
        &mut |len| {
            total_len.set(len);
            Ok(())
        },
        &mut |chunk| {
            file.write_all(chunk)?;
            received += chunk.len() as u64;
            progress(received, total_len.get());
            Ok(())
        },
    );
    drop(file);
    match result {
        Ok(()) => Ok(received),
        Err(e) => {
            let _ = std::fs::remove_file(dest);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// Local HTTP server answering `requests` requests with `status` and `body`.
    fn serve(
        status: &str,
        body: Vec<u8>,
        requests: usize,
    ) -> DeskResult<(String, std::thread::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let status = status.to_string();
        let handle = std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut head = [0u8; 2048];
                let _ = stream.read(&mut head);
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Ok((format!("http://127.0.0.1:{port}/file.bin"), handle))
    }

    #[test]
    fn urls_split_into_host_port_and_path() -> DeskResult<()> {
        let t = parse_target("https://api.github.com/repos/a/b?x=1")?;
        assert_eq!(
            (t.secure, t.host.as_str(), t.port, t.path.as_str()),
            (true, "api.github.com", 443, "/repos/a/b?x=1")
        );
        let t = parse_target("http://127.0.0.1:8080")?;
        assert_eq!((t.secure, t.port, t.path.as_str()), (false, 8080, "/"));
        for bad in [
            "ftp://x/y",
            "file:///c:/a",
            "http://",
            "http://u@h/",
            "http://h:99999/",
        ] {
            assert!(parse_target(bad).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn get_bytes_reads_the_body_and_caps_the_size() -> DeskResult<()> {
        let (url, server) = serve("200 OK", vec![7u8; 100_000], 2)?;
        assert_eq!(get_bytes(&url, 200_000)?.len(), 100_000);
        assert!(get_bytes(&url, 1_000).is_err(), "over the cap is refused");
        let _ = server.join();
        Ok(())
    }

    #[test]
    fn non_success_status_is_an_error() -> DeskResult<()> {
        let (url, server) = serve("404 Not Found", b"nope".to_vec(), 1)?;
        let err = get_bytes(&url, 1_000).err().map(|e| e.to_string());
        assert_eq!(err.as_deref(), Some("Transport error: http status 404"));
        let _ = server.join();
        Ok(())
    }

    #[test]
    fn download_writes_the_file_and_reports_progress_and_removes_it_on_failure() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let (url, server) = serve("200 OK", vec![1u8; 150_000], 2)?;
        let dest = dir.path().join("a.bin");
        let mut last = (0, None);
        let n = download(&url, &dest, 1_000_000, &mut |r, t| last = (r, t))?;
        assert_eq!((n, std::fs::metadata(&dest)?.len()), (150_000, 150_000));
        assert_eq!(last, (150_000, Some(150_000)));
        let small = dir.path().join("b.bin");
        assert!(download(&url, &small, 10, &mut |_, _| {}).is_err());
        assert!(!small.exists(), "a failed download leaves no file");
        let _ = server.join();
        Ok(())
    }

    #[test]
    fn nothing_listening_is_a_clean_error() -> DeskResult<()> {
        let url = {
            let l = TcpListener::bind("127.0.0.1:0")?;
            format!("http://127.0.0.1:{}/x", l.local_addr()?.port())
        };
        assert!(get_bytes(&url, 10).is_err());
        Ok(())
    }
}
