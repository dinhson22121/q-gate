//! Runner protocol over a Unix socket: one request per connection, each message one line of JSON.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::sandbox::domain::{ExecRequest, ExecResult, MAX_OUTPUT_BYTES};

/// Largest request line (argv + env are bounded far below this).
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Largest response line: the output tail is JSON-escaped (up to 6 bytes per byte) plus small fields.
pub const MAX_RESPONSE_BYTES: usize = MAX_OUTPUT_BYTES * 6 + 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerRequest {
    /// Health check: answered with [`RunnerResponse::Pong`].
    Ping,
    Exec {
        request: ExecRequest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunnerResponse {
    Pong,
    Done {
        result: ExecResult,
    },
    /// The message could not be parsed.
    Invalid {
        reason: String,
    },
}

/// Writes `message` as one JSON line and flushes.
pub async fn write_message<W: AsyncWrite + Unpin, T: Serialize>(writer: &mut W, message: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// Reads one JSON line of at most `max_bytes`. `Ok(None)` at a clean end of stream.
pub async fn read_message<R: AsyncBufRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<T>> {
    let mut line = Vec::new();
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        let reason = if line.len() > max_bytes { "message too large" } else { "connection closed mid-message" };
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, reason));
    }
    serde_json::from_slice(&line).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn messages_round_trip_as_json_lines() {
        let request = RunnerRequest::Exec {
            request: ExecRequest {
                argv: vec!["go".into(), "test".into()],
                cwd: PathBuf::from("/work/s"),
                env: BTreeMap::new(),
                timeout_ms: 5,
                max_output_bytes: 10,
            },
        };
        let mut buffer = Vec::new();
        write_message(&mut buffer, &request).await.unwrap();
        write_message(&mut buffer, &RunnerRequest::Ping).await.unwrap();
        assert_eq!(buffer.iter().filter(|b| **b == b'\n').count(), 2);
        let mut reader = BufReader::new(buffer.as_slice());
        assert_eq!(read_message::<_, RunnerRequest>(&mut reader, 1024).await.unwrap(), Some(request));
        assert_eq!(read_message::<_, RunnerRequest>(&mut reader, 1024).await.unwrap(), Some(RunnerRequest::Ping));
        assert_eq!(read_message::<_, RunnerRequest>(&mut reader, 1024).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_truncated_or_malformed_messages_are_errors() {
        let mut reader = BufReader::new(&b"{\"op\":\"ping\"}\n"[..]);
        let error = read_message::<_, RunnerRequest>(&mut reader, 4).await.unwrap_err();
        assert!(error.to_string().contains("too large"));
        let mut reader = BufReader::new(&b"{\"op\":\"ping\"}"[..]);
        let error = read_message::<_, RunnerRequest>(&mut reader, 1024).await.unwrap_err();
        assert!(error.to_string().contains("mid-message"));
        let mut reader = BufReader::new(&b"{\"op\":\"shell\"}\n"[..]);
        assert!(read_message::<_, RunnerRequest>(&mut reader, 1024).await.is_err());
    }
}
