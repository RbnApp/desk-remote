//! Full-duplex use of an authenticated [`Session`].
//!
//! A viewer streams video in while sending input and heartbeats out. [`Session::split`]
//! hands the read half and the write half to different threads; both share the Noise
//! transport behind a mutex that is only held while encrypting or decrypting, never
//! while waiting on the socket.

use crate::session::{seal_message, Session};
use desk_crypto::NoiseTransport;
use desk_error::{DeskError, DeskResult};
use desk_protocol::{FrameCodec, Message};
use std::io::Write;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};

type SharedTransport = Arc<Mutex<NoiseTransport>>;

fn lock(shared: &SharedTransport) -> MutexGuard<'_, NoiseTransport> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Receiving half of a split session.
pub struct SessionReader {
    raw: Session,
    shared: SharedTransport,
}

/// Sending half of a split session.
pub struct SessionWriter {
    stream: TcpStream,
    shared: SharedTransport,
}

impl Session {
    /// Splits an authenticated session into a reader and a writer for two threads.
    pub fn split(mut self) -> DeskResult<(SessionReader, SessionWriter)> {
        let transport = self
            .transport
            .take()
            .ok_or_else(|| DeskError::Transport("session is not authenticated".to_string()))?;
        let shared = Arc::new(Mutex::new(transport));
        let stream = self.stream.try_clone().map_err(DeskError::Io)?;
        Ok((
            SessionReader {
                raw: self,
                shared: Arc::clone(&shared),
            },
            SessionWriter { stream, shared },
        ))
    }
}

impl SessionReader {
    /// Blocks until the next complete message arrives (bounded by the session read timeout).
    pub fn recv(&mut self) -> DeskResult<Message> {
        loop {
            if let Some((msg, used)) = FrameCodec::decode(&self.raw.plain_buf)? {
                self.raw.plain_buf.drain(..used);
                return Ok(msg);
            }
            let ct = self.raw.read_raw()?;
            let plain = lock(&self.shared).decrypt(&ct)?;
            self.raw.plain_buf.extend_from_slice(&plain);
        }
    }
}

impl SessionWriter {
    /// Sends one message; call from a single thread at a time so nonces follow wire order.
    pub fn send(&mut self, msg: &Message) -> DeskResult<()> {
        let wire = seal_message(&mut lock(&self.shared), msg)?;
        self.stream.write_all(&wire).map_err(DeskError::Io)?;
        self.stream.flush().map_err(DeskError::Io)
    }

    /// Closes the connection in both directions, which also ends a blocked reader.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
