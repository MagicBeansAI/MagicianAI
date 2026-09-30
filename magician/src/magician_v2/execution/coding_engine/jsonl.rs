//! Bounded JSONL helpers for Codex stdio sessions.
//!
//! Read a line only up to `max_line_bytes` so a flood cannot grow the heap
//! first and check the cap afterwards. Depth is walked iteratively and only
//! into nested containers, so a wide scalar array cannot blow the stack or
//! the walker heap.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedJsonlError {
    Eof,
    Io,
    Oversized,
    Malformed,
    TooDeep,
}

pub async fn read_bounded_jsonl_value<R>(
    reader: &mut R,
    max_line_bytes: usize,
    max_json_depth: usize,
) -> Result<Value, BoundedJsonlError>
where
    R: AsyncBufRead + Unpin,
{
    let raw = read_bounded_jsonl_line(reader, max_line_bytes).await?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| BoundedJsonlError::Malformed)?;
    if json_depth_exceeds(&value, max_json_depth) {
        return Err(BoundedJsonlError::TooDeep);
    }
    Ok(value)
}

/// Cancellation-safe bounded JSONL read for callers that multiplex the input
/// stream with control channels. Bytes consumed before an await live in the
/// caller-owned `partial` buffer, so cancelling this future cannot discard the
/// prefix of a fragmented JSON record.
pub async fn read_bounded_jsonl_value_buffered<R>(
    reader: &mut R,
    partial: &mut Vec<u8>,
    max_line_bytes: usize,
    max_json_depth: usize,
) -> Result<Value, BoundedJsonlError>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await.map_err(|_| BoundedJsonlError::Io)?;
        if available.is_empty() {
            if partial.is_empty() {
                return Err(BoundedJsonlError::Eof);
            }
            break;
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if partial.len().saturating_add(newline) > max_line_bytes {
                reader.consume(newline + 1);
                partial.clear();
                return Err(BoundedJsonlError::Oversized);
            }
            partial.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            break;
        }
        if partial.len().saturating_add(available.len()) > max_line_bytes {
            let take = available.len();
            reader.consume(take);
            partial.clear();
            return Err(BoundedJsonlError::Oversized);
        }
        let take = available.len();
        partial.extend_from_slice(available);
        reader.consume(take);
    }

    if partial.ends_with(&[b'\r']) {
        partial.pop();
    }
    let raw = std::mem::take(partial);
    let value: Value = serde_json::from_slice(&raw).map_err(|_| BoundedJsonlError::Malformed)?;
    if json_depth_exceeds(&value, max_json_depth) {
        return Err(BoundedJsonlError::TooDeep);
    }
    Ok(value)
}

pub async fn read_bounded_jsonl_line<R>(
    reader: &mut R,
    max_line_bytes: usize,
) -> Result<Vec<u8>, BoundedJsonlError>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|_| BoundedJsonlError::Io)?;
        if available.is_empty() {
            if buf.is_empty() {
                return Err(BoundedJsonlError::Eof);
            }
            break;
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if buf.len().saturating_add(newline) > max_line_bytes {
                reader.consume(newline + 1);
                return Err(BoundedJsonlError::Oversized);
            }
            buf.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            break;
        }
        if buf.len().saturating_add(available.len()) > max_line_bytes {
            let take = available.len();
            reader.consume(take);
            drain_through_newline(reader).await;
            return Err(BoundedJsonlError::Oversized);
        }
        let take = available.len();
        buf.extend_from_slice(available);
        reader.consume(take);
    }
    if buf.ends_with(&[b'\r']) {
        buf.pop();
    }
    Ok(buf)
}

async fn drain_through_newline<R>(reader: &mut R)
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let Ok(available) = reader.fill_buf().await else {
            return;
        };
        if available.is_empty() {
            return;
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            reader.consume(newline + 1);
            return;
        }
        let take = available.len();
        reader.consume(take);
    }
}

/// True when the value nests deeper than `max_depth`. Only container nodes
/// are stacked, so a wide array of scalars stays O(1) walker memory.
pub fn json_depth_exceeds(value: &Value, max_depth: usize) -> bool {
    let mut stack = vec![(value, 1usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            return true;
        }
        match node {
            Value::Array(items) => {
                for item in items {
                    if item.is_array() || item.is_object() {
                        stack.push((item, depth + 1));
                    }
                }
            },
            Value::Object(map) => {
                for item in map.values() {
                    if item.is_array() || item.is_object() {
                        stack.push((item, depth + 1));
                    }
                }
            },
            _ => {},
        }
    }
    false
}

pub fn append_bounded_utf8(buffer: &mut String, text: &str, max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(buffer.len());
    if remaining == 0 || text.is_empty() {
        return;
    }
    let mut end = remaining.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    buffer.push_str(&text[..end]);
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{future::Future, task::Poll};
    use tokio::io::{AsyncWriteExt, BufReader};

    #[test]
    fn depth_walker_rejects_past_the_cap_without_walking_scalars() {
        let nested = json!({"a":{"b":{"c":1}}});
        assert!(!json_depth_exceeds(&nested, 3));
        assert!(json_depth_exceeds(&nested, 2));
        let wide = Value::Array((0..10_000).map(Value::from).collect());
        assert!(!json_depth_exceeds(&wide, 2));
        assert!(!json_depth_exceeds(&wide, 1));
    }

    #[test]
    fn utf8_append_does_not_split_a_codepoint() {
        let mut buffer = String::new();
        append_bounded_utf8(&mut buffer, "éé", 3);
        assert_eq!(buffer, "é");
    }

    #[tokio::test]
    async fn oversized_line_is_rejected_before_the_whole_line_is_kept() {
        let payload = format!("{}\n", "x".repeat(64));
        let mut reader = BufReader::new(payload.as_bytes());
        let error = read_bounded_jsonl_line(&mut reader, 16)
            .await
            .expect_err("oversized");
        assert_eq!(error, BoundedJsonlError::Oversized);
    }

    #[tokio::test]
    async fn coalesced_then_complete_line_parses() {
        let mut reader = BufReader::new(&b"{\"ok\":true}\n"[..]);
        let value = read_bounded_jsonl_value(&mut reader, 64, 8)
            .await
            .expect("json");
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn buffered_read_keeps_a_fragment_when_the_future_is_cancelled() {
        let (client, mut peer) = tokio::io::duplex(256);
        let prefix = b"{\"ok\":";
        peer.write_all(prefix).await.expect("prefix");
        let mut reader = BufReader::new(client);
        let mut partial = Vec::new();

        let mut read = Box::pin(read_bounded_jsonl_value_buffered(
            &mut reader,
            &mut partial,
            64,
            8,
        ));
        std::future::poll_fn(|cx| match read.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(result) => panic!("fragmented read settled early: {result:?}"),
        })
        .await;
        drop(read);
        assert_eq!(partial, prefix);

        peer.write_all(b"true}\n").await.expect("suffix");
        let value = read_bounded_jsonl_value_buffered(&mut reader, &mut partial, 64, 8)
            .await
            .expect("resumed JSONL read");
        assert_eq!(value["ok"], true);
        assert!(partial.is_empty());
    }
}
