use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Read a single LSP message from the reader.
/// Parses `Content-Length` header and reads the JSON body.
pub async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;

    // Parse headers
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(None); // EOF
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }

        if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
            content_length = Some(
                len_str
                    .parse()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            );
        }
        // Ignore other headers (e.g., Content-Type)
    }

    let length = content_length
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length header"))?;

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;

    let value: Value = serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    Ok(Some(value))
}

/// Write a single LSP message to the writer.
/// Prepends `Content-Length` header.
pub async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> std::io::Result<()> {
    let body = serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_roundtrip() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1,
            "params": {}
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn test_roundtrip_unicode() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/hover",
            "params": { "text": "こんにちは世界 🌍" }
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn test_eof_returns_none() {
        let empty: &[u8] = &[];
        let mut reader = BufReader::new(empty);
        let result = read_message(&mut reader).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_multiple_messages() {
        let msg1 = serde_json::json!({"jsonrpc":"2.0","method":"a"});
        let msg2 = serde_json::json!({"jsonrpc":"2.0","method":"b"});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg1).await.unwrap();
        write_message(&mut buf, &msg2).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let r1 = read_message(&mut reader).await.unwrap().unwrap();
        let r2 = read_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(r1, msg1);
        assert_eq!(r2, msg2);
    }

    #[tokio::test]
    async fn test_large_message() {
        let large_text = "x".repeat(100_000);
        let msg = serde_json::json!({"data": large_text});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await.unwrap().unwrap();

        assert_eq!(result, msg);
    }
}
