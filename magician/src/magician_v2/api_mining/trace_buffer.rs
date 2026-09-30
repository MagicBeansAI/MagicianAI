//! Ring buffer for storing network traces per tab
//!
//! Implements a fixed-size ring buffer with 100 entry limit and 1MB per entry limit.
//! Designed for minimal memory overhead and lock-free reads where possible.

use super::types::{NetworkTraceEvent, TraceCaptureStats};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Maximum number of traces per tab (aligned with browser extension's 200 limit)
const MAX_TRACES_PER_TAB: usize = 200;

/// Maximum size per trace entry (1MB)
const MAX_TRACE_SIZE_BYTES: usize = 1_024_000;

/// Find the largest byte position ≤ `max_bytes` that falls on a UTF-8 char boundary.
/// Prevents panics when slicing multi-byte strings at arbitrary byte offsets.
fn safe_truncate_pos(s: &str, max_bytes: usize) -> usize {
    let pos = s.len().min(max_bytes);
    // Walk backwards from `pos` until we hit a char boundary
    (0..=pos)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

/// Ring buffer for network traces
pub struct TraceBuffer {
    /// Internal buffer storage
    buffer: Arc<RwLock<VecDeque<NetworkTraceEvent>>>,

    /// Capture statistics
    stats: Arc<RwLock<TraceCaptureStats>>,

    /// Tab ID this buffer belongs to
    tab_id: String,
}

impl TraceBuffer {
    /// Create a new trace buffer for a tab
    pub fn new(tab_id: String) -> Self {
        Self {
            buffer: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_TRACES_PER_TAB))),
            stats: Arc::new(RwLock::new(TraceCaptureStats::default())),
            tab_id,
        }
    }

    /// Add a network trace event to the buffer
    pub fn push(&self, mut event: NetworkTraceEvent) -> Result<(), String> {
        let start = Instant::now();

        // Check size limits and truncate if needed
        let event_size = self.estimate_event_size(&event);
        let mut was_oversized = false;
        if event_size > MAX_TRACE_SIZE_BYTES {
            // Keep a bounded prefix rather than a token preview. Collapsing an
            // oversized body to a kilobyte made the cap a cliff: a rendered
            // document measured 34 bytes over the limit and lost everything a
            // recipe could have been compiled from, while a script just under
            // it was kept whole. The prefix is where a document states what it
            // is about, so half the budget per body keeps the event inside its
            // bound and still leaves something to mine.
            const KEEP_BYTES: usize = MAX_TRACE_SIZE_BYTES / 2;
            if let Some(ref body) = event.request_body {
                if body.len() > KEEP_BYTES {
                    let truncate_at = safe_truncate_pos(body, KEEP_BYTES);
                    event.request_body = Some(format!(
                        "{}... [TRUNCATED: {} bytes]",
                        &body[..truncate_at],
                        body.len()
                    ));
                }
            }
            if let Some(ref body) = event.response_body {
                if body.len() > KEEP_BYTES {
                    let truncate_at = safe_truncate_pos(body, KEEP_BYTES);
                    event.response_body = Some(format!(
                        "{}... [TRUNCATED: {} bytes]",
                        &body[..truncate_at],
                        body.len()
                    ));
                }
            }
            was_oversized = true;
        }

        // Hold buffer lock only for ring-buffer operations, then release
        // before acquiring the stats lock (prevents AB/BA deadlock).
        let mut was_buffer_full = false;
        {
            let mut buffer = self.buffer.write().map_err(|e| e.to_string())?;
            if buffer.len() >= MAX_TRACES_PER_TAB {
                buffer.pop_front();
                was_buffer_full = true;
            }
            buffer.push_back(event);
        } // buffer lock released

        // Single stats lock acquisition for all updates
        let elapsed = start.elapsed();
        let mut stats = self.stats.write().map_err(|e| e.to_string())?;
        if was_oversized {
            stats.dropped_oversized += 1;
        }
        if was_buffer_full {
            stats.dropped_buffer_full += 1;
        }
        stats.total_requests += 1;
        stats.total_bytes_captured += event_size;

        let overhead_us = elapsed.as_micros() as f64;
        stats.avg_overhead_us = (stats.avg_overhead_us * (stats.total_requests - 1) as f64
            + overhead_us)
            / stats.total_requests as f64;

        if overhead_us > stats.max_overhead_us {
            stats.max_overhead_us = overhead_us;
        }

        Ok(())
    }

    /// Get all traces from the buffer
    pub fn get_all(&self) -> Result<Vec<NetworkTraceEvent>, String> {
        let buffer = self.buffer.read().map_err(|e| e.to_string())?;
        Ok(buffer.iter().cloned().collect())
    }

    /// Get the most recent N traces
    pub fn get_recent(&self, n: usize) -> Result<Vec<NetworkTraceEvent>, String> {
        let buffer = self.buffer.read().map_err(|e| e.to_string())?;
        let start = if buffer.len() > n {
            buffer.len() - n
        } else {
            0
        };
        Ok(buffer.iter().skip(start).cloned().collect())
    }

    /// Clear all traces from the buffer
    pub fn clear(&self) -> Result<(), String> {
        let mut buffer = self.buffer.write().map_err(|e| e.to_string())?;
        buffer.clear();
        Ok(())
    }

    /// Get current buffer statistics
    pub fn get_stats(&self) -> Result<TraceCaptureStats, String> {
        let stats = self.stats.read().map_err(|e| e.to_string())?;
        Ok(stats.clone())
    }

    /// Get the tab ID this buffer belongs to
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    /// Get current buffer size
    pub fn len(&self) -> Result<usize, String> {
        let buffer = self.buffer.read().map_err(|e| e.to_string())?;
        Ok(buffer.len())
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> Result<bool, String> {
        Ok(self.len()? == 0)
    }

    /// Estimate size of a trace event in bytes
    fn estimate_event_size(&self, event: &NetworkTraceEvent) -> usize {
        let mut size = 0;
        size += event.request_id.len();
        size += event.method.len();
        size += event.url.len();
        size += event
            .request_headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>();
        size += event.request_body.as_ref().map_or(0, |b| b.len());
        size += event
            .response_headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>();
        size += event.response_body.as_ref().map_or(0, |b| b.len());
        size
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn create_test_event(id: &str, size: usize) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: id.to_string(),
            method: "GET".to_string(),
            url: "https://example.com".to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::new(),
            request_body: Some("x".repeat(size)),
            response_headers: HashMap::new(),
            response_body: None,
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: super::super::types::RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 100.0,
            },
            initiator: super::super::types::RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: 0,
            request_size: size as u64,
            response_size: 0,
            capture_source: None,
        }
    }

    #[test]
    fn test_buffer_creation() {
        let buffer = TraceBuffer::new("tab1".to_string());
        assert_eq!(buffer.tab_id(), "tab1");
        assert!(buffer.is_empty().unwrap());
    }

    #[test]
    fn test_buffer_push_and_get() {
        let buffer = TraceBuffer::new("tab1".to_string());
        let event = create_test_event("req1", 100);

        buffer.push(event.clone()).unwrap();
        assert_eq!(buffer.len().unwrap(), 1);

        let traces = buffer.get_all().unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].request_id, "req1");
    }

    #[test]
    fn test_buffer_overflow() {
        let buffer = TraceBuffer::new("tab1".to_string());

        // Push more than MAX_TRACES_PER_TAB
        for i in 0..250 {
            let event = create_test_event(&format!("req{}", i), 100);
            buffer.push(event).unwrap();
        }

        // Should only keep last 200
        assert_eq!(buffer.len().unwrap(), MAX_TRACES_PER_TAB);

        let traces = buffer.get_all().unwrap();
        assert_eq!(traces[0].request_id, "req50"); // First 50 dropped
    }

    #[test]
    fn test_oversized_truncation() {
        let buffer = TraceBuffer::new("tab1".to_string());
        let event = create_test_event("req1", MAX_TRACE_SIZE_BYTES + 1000);

        buffer.push(event).unwrap();

        let stats = buffer.get_stats().unwrap();
        assert_eq!(stats.dropped_oversized, 1);
    }

    #[test]
    fn test_safe_truncate_pos_ascii() {
        // ASCII: each char is 1 byte
        assert_eq!(safe_truncate_pos("Hello", 3), 3);
        assert_eq!(safe_truncate_pos("Hello", 100), 5);
        assert_eq!(safe_truncate_pos("Hello", 0), 0);
        assert_eq!(safe_truncate_pos("", 10), 0);
    }

    #[test]
    fn test_safe_truncate_pos_multibyte() {
        // Emoji: 4 bytes each.  "Hi🌍" = [72, 105, 240, 159, 140, 141]
        let s = "Hi\u{1F30D}"; // "Hi🌍"
        assert_eq!(s.len(), 6);
        // Truncating at byte 4 is mid-emoji → should snap back to byte 2
        assert_eq!(safe_truncate_pos(s, 4), 2);
        // Truncating at byte 6 = exact end of emoji
        assert_eq!(safe_truncate_pos(s, 6), 6);
        // Truncating at byte 2 = just after "Hi"
        assert_eq!(safe_truncate_pos(s, 2), 2);
    }

    #[test]
    fn test_safe_truncate_pos_cjk() {
        // CJK characters: 3 bytes each. "你好" = 6 bytes
        let s = "你好";
        assert_eq!(s.len(), 6);
        // Byte 1 is mid-character → snap to 0
        assert_eq!(safe_truncate_pos(s, 1), 0);
        // Byte 3 = end of first char
        assert_eq!(safe_truncate_pos(s, 3), 3);
        // Byte 4 is mid second char → snap to 3
        assert_eq!(safe_truncate_pos(s, 4), 3);
    }

    #[test]
    fn a_body_just_over_the_cap_keeps_enough_to_mine() {
        // Measured live: a rendered article arrived 34 bytes over the cap and
        // was stored as a kilobyte, losing the value a recipe needed, while a
        // script just under the cap was kept whole. The answer sits near the
        // top of such a document, so a prefix has to survive the cliff.
        let marker = "FIRST_APPEARED_2012";
        let mut body = "a".repeat(17_000);
        body.push_str(marker);
        body.push_str(&"b".repeat(MAX_TRACE_SIZE_BYTES + 34 - body.len()));
        assert!(body.len() > MAX_TRACE_SIZE_BYTES);

        let buffer = TraceBuffer::new("tab1".to_string());
        let mut event = create_test_event("req1", 0);
        event.request_body = None;
        event.response_body = Some(body.clone());
        buffer.push(event).unwrap();

        let stored = buffer.get_all().unwrap()[0]
            .response_body
            .clone()
            .expect("a body");
        assert!(
            stored.contains(marker),
            "the mineable prefix was dropped: kept {} bytes",
            stored.len()
        );
        assert!(
            stored.len() <= MAX_TRACE_SIZE_BYTES,
            "the event must stay inside its budget: {} bytes",
            stored.len()
        );
    }
}
