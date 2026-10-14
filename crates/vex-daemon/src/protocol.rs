//! The wire protocol between vex clients and the daemon: each frame is a
//! big-endian u32 byte length followed by that many bytes of JSON. Every
//! connection opens with `hello`; a daemon of another version or schema
//! answers `mismatch` and drains.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The protocol schema both sides must share.
pub const SCHEMA: u32 = 1;
/// The largest frame either side accepts.
pub const MAX_FRAME: usize = 64 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The first frame of every connection.
    Hello {
        version: String,
        schema: u32,
    },
    Ping,
    /// The daemon's pid, version, uptime and the handler's own report.
    Status,
    /// Stop serving and exit.
    Shutdown,
    /// A command for the handler (the CLI or MCP adapter's request).
    Call {
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Welcome {
        version: String,
        schema: u32,
        pid: u32,
    },
    /// The daemon is another version: it drains and exits.
    Mismatch {
        daemon_version: String,
        daemon_schema: u32,
    },
    Pong,
    Status {
        status: Value,
    },
    ShuttingDown,
    Reply {
        payload: Value,
    },
    Error {
        message: String,
    },
}

fn too_big(len: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("frame of {len} bytes exceeds the {MAX_FRAME}-byte limit"),
    )
}

fn encode<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(too_big(body.len()));
    }
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    serde_json::from_slice(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    w.write_all(&encode(msg)?)?;
    w.flush()
}

pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(too_big(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    decode(&body)
}

pub async fn write_frame_async<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
) -> io::Result<()> {
    w.write_all(&encode(msg)?).await?;
    w.flush().await
}

pub async fn read_frame_async<R: AsyncRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(too_big(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    decode(&body)
}
