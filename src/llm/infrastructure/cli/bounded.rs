use std::io::{Error, ErrorKind, Result};

use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub async fn read_capped(stream: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.take(cap as u64 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > cap {
        return Err(Error::new(ErrorKind::FileTooLarge, format!("output exceeds {cap} bytes")));
    }
    Ok(bytes)
}

pub fn exceeded_cap(error: &Error) -> bool {
    error.kind() == ErrorKind::FileTooLarge
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_everything_up_to_the_cap() {
        let data = vec![b'a'; 10];
        assert_eq!(read_capped(&data[..], 10).await.unwrap(), data);
        assert!(read_capped(&b""[..], 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stops_reading_one_byte_past_the_cap() {
        struct Endless {
            served: usize,
        }
        impl AsyncRead for Endless {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<Result<()>> {
                let n = buf.remaining();
                buf.put_slice(&vec![b'x'; n]);
                self.served += n;
                std::task::Poll::Ready(Ok(()))
            }
        }
        let mut stream = Endless { served: 0 };

        let error = read_capped(&mut stream, 1000).await.unwrap_err();

        assert!(exceeded_cap(&error));
        assert!(stream.served <= 1001 + 8192, "served {}", stream.served);
    }
}
