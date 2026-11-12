//! Real named-pipe tests: duplex traffic, concurrency, and DACL enforcement.

use super::*;
use desk_protocol::Message;

fn unique_pipe(tag: &str) -> String {
    format!(r"\\.\pipe\DeskRemote-test-{tag}-{}", std::process::id())
}

fn allow_current_user() -> DeskResult<String> {
    Ok(pipe_access_sddl(&current_user_sid()?))
}

fn no_client() -> DeskError {
    DeskError::Ipc("no client".to_string())
}

#[test]
fn duplex_roundtrip_with_large_frames() -> DeskResult<()> {
    let name = unique_pipe("duplex");
    let server = IpcServer::bind(&name, &allow_current_user()?)?;
    let client_name = name.clone();
    let client = std::thread::spawn(move || -> DeskResult<()> {
        let conn = IpcClient::connect(&client_name, Duration::from_secs(5))?;
        conn.send(&IpcMessage::HelperHello {
            session_id: 7,
            pid: 1,
        })?;
        let big = conn.recv_timeout(Duration::from_secs(5))?;
        assert!(matches!(
            big,
            Some(IpcMessage::Wire(Message::VideoFrame { ref data, .. })) if data.len() == 3_000_000
        ));
        conn.send(&IpcMessage::NoDisplay)?;
        Ok(())
    });
    let conn = server
        .accept(Duration::from_secs(5))?
        .ok_or_else(no_client)?;
    assert_eq!(
        conn.recv_timeout(Duration::from_secs(5))?,
        Some(IpcMessage::HelperHello {
            session_id: 7,
            pid: 1
        })
    );
    assert_eq!(conn.client_process_id()?, std::process::id());
    conn.send(&IpcMessage::Wire(Message::VideoFrame {
        ts_us: 1,
        keyframe: true,
        data: vec![0xAB; 3_000_000],
    }))?;
    assert_eq!(
        conn.recv_timeout(Duration::from_secs(5))?,
        Some(IpcMessage::NoDisplay)
    );
    client
        .join()
        .map_err(|_| DeskError::Ipc("client panicked".to_string()))??;
    Ok(())
}

#[test]
fn read_and_write_proceed_concurrently_on_one_connection() -> DeskResult<()> {
    let name = unique_pipe("concurrent");
    let server = IpcServer::bind(&name, &allow_current_user()?)?;
    let client = IpcClient::connect(&name, Duration::from_secs(5))?;
    let conn = server
        .accept(Duration::from_secs(5))?
        .ok_or_else(no_client)?;

    // A reader blocked in recv_timeout on the client must not stop the client from writing.
    let reader = client.clone();
    let blocked = std::thread::spawn(move || reader.recv_timeout(Duration::from_secs(3)));
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    client.send(&IpcMessage::PairingCodeRequest)?;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "write was blocked by the read"
    );
    assert_eq!(
        conn.recv_timeout(Duration::from_secs(2))?,
        Some(IpcMessage::PairingCodeRequest)
    );
    conn.send(&IpcMessage::Shutdown)?;
    let got = blocked
        .join()
        .map_err(|_| DeskError::Ipc("reader panicked".to_string()))??;
    assert_eq!(got, Some(IpcMessage::Shutdown));
    Ok(())
}

#[test]
fn recv_times_out_without_losing_state() -> DeskResult<()> {
    let name = unique_pipe("timeout");
    let server = IpcServer::bind(&name, &allow_current_user()?)?;
    let client = IpcClient::connect(&name, Duration::from_secs(5))?;
    let conn = server
        .accept(Duration::from_secs(5))?
        .ok_or_else(no_client)?;
    assert_eq!(conn.recv_timeout(Duration::from_millis(50))?, None);
    client.send(&IpcMessage::NoDisplay)?;
    assert_eq!(
        conn.recv_timeout(Duration::from_secs(2))?,
        Some(IpcMessage::NoDisplay)
    );
    client.close();
    drop(client);
    assert!(conn.recv_timeout(Duration::from_secs(2)).is_err());
    Ok(())
}

#[test]
fn red_acl_without_the_user_denies_the_connection() -> DeskResult<()> {
    let name = unique_pipe("acl");
    // SYSTEM only: the current (non-SYSTEM) user must not be able to open the pipe.
    let _server = IpcServer::bind(&name, "D:P(A;;GA;;;SY)")?;
    match IpcClient::connect(&name, Duration::from_secs(1)) {
        Err(DeskError::Ipc(msg)) => assert!(
            msg.contains("refused"),
            "expected an access refusal, got: {msg}"
        ),
        Err(other) => return Err(other),
        Ok(_) => return Err(DeskError::Ipc("pipe ACL was not enforced".to_string())),
    }
    Ok(())
}

#[test]
fn red_second_server_cannot_squat_the_pipe_name() -> DeskResult<()> {
    let name = unique_pipe("squat");
    let _first = IpcServer::bind(&name, &allow_current_user()?)?;
    assert!(IpcServer::bind(&name, &allow_current_user()?).is_err());
    Ok(())
}

#[test]
fn accept_times_out_when_nobody_connects() -> DeskResult<()> {
    let name = unique_pipe("idle");
    let server = IpcServer::bind(&name, &allow_current_user()?)?;
    assert!(server.accept(Duration::from_millis(100))?.is_none());
    // The pipe is still usable afterwards.
    let _client = IpcClient::connect(&name, Duration::from_secs(2))?;
    assert!(server.accept(Duration::from_secs(2))?.is_some());
    Ok(())
}

/// Connects one client to a fresh server and returns the server side of the connection.
fn connected(tag: &str) -> DeskResult<(IpcConnection, IpcConnection)> {
    let name = unique_pipe(tag);
    let server = IpcServer::bind(&name, &allow_current_user()?)?;
    let client = IpcClient::connect(&name, Duration::from_secs(5))?;
    let conn = server
        .accept(Duration::from_secs(5))?
        .ok_or_else(no_client)?;
    Ok((conn, client))
}

#[test]
fn verify_client_accepts_the_expected_process() -> DeskResult<()> {
    let (conn, _client) = connected("verify-ok")?;
    let me = std::env::current_exe()?;
    conn.verify_client(&ExpectedClient {
        pid: std::process::id(),
        image: &me,
        user_sid: &current_user_sid()?,
    })
}

#[test]
fn verify_client_rejects_a_wrong_executable() -> DeskResult<()> {
    let (conn, _client) = connected("verify-exe")?;
    let other = desk_paths::DeskPaths::system_tool("cmd.exe");
    let err = conn.verify_client(&ExpectedClient {
        pid: std::process::id(),
        image: &other,
        user_sid: &current_user_sid()?,
    });
    assert!(format!("{err:?}").contains("instead of"), "{err:?}");
    Ok(())
}

#[test]
fn verify_client_rejects_a_wrong_pid_and_a_wrong_user() -> DeskResult<()> {
    let (conn, _client) = connected("verify-pid")?;
    let me = std::env::current_exe()?;
    let sid = current_user_sid()?;
    let wrong_pid = conn.verify_client(&ExpectedClient {
        pid: std::process::id() + 4,
        image: &me,
        user_sid: &sid,
    });
    assert!(wrong_pid.is_err());
    let wrong_user = conn.verify_client(&ExpectedClient {
        pid: std::process::id(),
        image: &me,
        user_sid: "S-1-5-18",
    });
    assert!(
        format!("{wrong_user:?}").contains("another user"),
        "{wrong_user:?}"
    );
    Ok(())
}
