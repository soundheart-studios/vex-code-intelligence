//! The blocking client the CLI and MCP adapters use: connect, say hello,
//! send requests. No async runtime needed on the client side.

use crate::protocol::{Request, Response, SCHEMA, read_frame, write_frame};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A connection that has completed the handshake.
#[derive(Debug)]
pub struct Conn {
    stream: UnixStream,
    pub daemon_version: String,
    pub pid: u32,
}

#[derive(Debug)]
pub enum Connect {
    Ready(Conn),
    /// No socket, or a stale one nobody listens on.
    NotRunning,
    /// The daemon is another version; it is draining and will exit.
    Mismatch {
        daemon_version: String,
    },
    Failed(String),
}

/// How long a handshake may take before the daemon counts as unresponsive.
const HANDSHAKE: Duration = Duration::from_secs(5);

pub fn connect(socket: &Path, version: &str) -> Connect {
    let mut stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Connect::NotRunning;
        }
        Err(e) => return Connect::Failed(e.to_string()),
    };
    let _ = stream.set_read_timeout(Some(HANDSHAKE));
    let hello = Request::Hello {
        version: version.to_string(),
        schema: SCHEMA,
    };
    let answer = write_frame(&mut stream, &hello).and_then(|_| read_frame(&mut stream));
    let _ = stream.set_read_timeout(None);
    match answer {
        Ok(Response::Welcome { version, pid, .. }) => Connect::Ready(Conn {
            stream,
            daemon_version: version,
            pid,
        }),
        Ok(Response::Mismatch { daemon_version, .. }) => Connect::Mismatch { daemon_version },
        Ok(other) => Connect::Failed(format!("unexpected handshake answer: {other:?}")),
        Err(e) => Connect::Failed(e.to_string()),
    }
}

impl Conn {
    pub fn request(&mut self, req: &Request) -> io::Result<Response> {
        write_frame(&mut self.stream, req)?;
        read_frame(&mut self.stream)
    }

    /// Bound how long a request may wait for its answer.
    pub fn set_timeout(&self, limit: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(limit)
    }
}
