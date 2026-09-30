# Phase 1 Integration Guide

## Overview

This document describes how to integrate the trace capture system into the existing Magician execution pipeline.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Browser Extension                         │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  debugger.js (to be implemented)                      │  │
│  │  - CDP Network.enable                                 │  │
│  │  - Event listeners (requestWillBeSent, etc.)         │  │
│  │  - Network.getResponseBody calls                     │  │
│  │  - Ring buffer (100 entries, 1MB max)                │  │
│  └──────────────────────────────────────────────────────┘  │
│                           │                                   │
│                           │ NetworkTraceEvent[]               │
│                           ▼                                   │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  Observe Action Handler                               │  │
│  │  - Returns traces when network_trace: true           │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                           │
                           │ WebSocket/IPC
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    Magician Core (Rust)                      │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  magicutor_client.rs (to be extended)                │  │
│  │  ┌────────────────────────────────────────────────┐  │  │
│  │  │  observe_with_trace() method                   │  │  │
│  │  │  - Sends observe request with network_trace    │  │  │
│  │  │  - Receives NetworkTraceEvent[]                │  │  │
│  │  └────────────────────────────────────────────────┘  │  │
│  └──────────────────────────────────────────────────────┘  │
│                           │                                   │
│                           ▼                                   │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  TraceManager                                         │  │
│  │  ┌────────────────────────────────────────────────┐  │  │
│  │  │  Per-tab TraceBuffer instances                 │  │  │
│  │  │  - add_trace()                                 │  │  │
│  │  │  - get_traces()                                │  │  │
│  │  │  - flush_traces()                              │  │  │
│  │  └────────────────────────────────────────────────┘  │  │
│  └──────────────────────────────────────────────────────┘  │
│                           │                                   │
│                           │ Periodic flush                    │
│                           ▼                                   │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  TraceStorage                                         │  │
│  │  - write_traces(task_id, traces[])                   │  │
│  │  - append_traces(task_id, traces[])                  │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼
       storage/magician/api_mining/traces/
         {task_id}/trace_{timestamp}.jsonl
```

## Integration Steps

### 1. Extension Side (JavaScript)

**File**: `magicutor/extension/debugger.js`

See `EXTENSION_TRACE_CAPTURE_SPEC.md` for detailed implementation spec.

Key additions:
- CDP Network domain enablement
- Event listeners for Network.* events
- Ring buffer implementation (TraceBuffer class)
- Extension to Observe action handler

### 2. Client Integration (Rust)

**File**: `magician/src/magician_v2/execution/magicutor_client.rs`

Add new method:

```rust
use crate::magician_v2::api_mining::types::{ObserveRequest, ObserveResponse};

impl MagicutorClient {
    /// Observe with network trace capture enabled
    pub async fn observe_with_trace(
        &self,
        tab_id: &str,
        max_traces: Option<usize>,
    ) -> Result<ObserveResponse, String> {
        let request = ObserveRequest {
            base: serde_json::json!({
                "tab_id": tab_id,
                // ... other observe parameters
            }),
            network_trace: true,
            max_traces: max_traces.unwrap_or(100),
        };
        
        let response = self.send_command("observe", &request).await?;
        Ok(response)
    }
}
```

### 3. Action Executor Integration

**File**: `magician/src/magician_v2/execution/action_executor.rs`

Modify Observe action handling:

```rust
use crate::magician_v2::api_mining::trace_manager::TraceManager;

pub struct ActionExecutor {
    magicutor_client: Arc<MagicutorClient>,
    trace_manager: Arc<TraceManager>,
    // ... other fields
}

impl ActionExecutor {
    pub async fn execute_observe(
        &self,
        action: &ObserveAction,
    ) -> Result<ObserveResult, String> {
        // Determine if we should capture traces
        let should_capture = self.should_capture_traces(action);
        
        let response = if should_capture {
            self.magicutor_client
                .observe_with_trace(&action.tab_id, Some(100))
                .await?
        } else {
            self.magicutor_client
                .observe(&action.tab_id)
                .await?
        };
        
        // If traces were captured, add them to trace manager
        if let Some(traces) = response.network_traces {
            for trace in traces {
                self.trace_manager.add_trace(&action.tab_id, trace)?;
            }
        }
        
        Ok(self.process_observe_response(response))
    }
    
    fn should_capture_traces(&self, action: &ObserveAction) -> bool {
        // Enable trace capture during automation sessions
        // Disable for simple queries
        // Could be controlled by config or task type
        true // Default: always capture in Phase 1
    }
}
```

### 4. Task Lifecycle Integration

**File**: `magician/src/magician_v2/execution/task_executor.rs`

Add trace flushing at task boundaries:

```rust
impl TaskExecutor {
    pub async fn execute_task(&self, task: &Task) -> Result<TaskResult, String> {
        // Set task ID in trace manager
        self.trace_manager.set_task_id(task.id.clone())?;
        
        // Execute task actions...
        let result = self.execute_actions(task).await?;
        
        // Flush traces to storage
        let trace_count = self.trace_manager.flush_all()?;
        println!("Flushed {} traces for task {}", trace_count, task.id);
        
        // Log statistics
        let stats = self.trace_manager.get_aggregate_stats()?;
        println!("Trace capture stats: {:?}", stats);
        
        Ok(result)
    }
}
```

### 5. Module Registration

**File**: `magician/src/magician_v2/mod.rs`

Add api_mining module:

```rust
pub mod api_mining;

// Export key types
pub use api_mining::{
    types::{NetworkTraceEvent, TraceCaptureStats},
    trace_manager::TraceManager,
};
```

### 6. Service Setup

**File**: `magician/src/magician_v2/service.rs`

Initialize trace manager:

```rust
use crate::magician_v2::api_mining::trace_manager::TraceManager;

pub struct MagicianService {
    trace_manager: Arc<TraceManager>,
    // ... other fields
}

impl MagicianService {
    pub fn new() -> Self {
        Self {
            trace_manager: Arc::new(TraceManager::new()),
            // ... other fields
        }
    }
}
```

## Configuration

**File**: `magician-config.yaml`

Add trace capture configuration:

```yaml
api_mining:
  phase1_trace_capture:
    enabled: true
    max_traces_per_tab: 100
    max_trace_size_bytes: 1048576  # 1MB
    flush_interval_seconds: 60
    storage_path: "storage/magician/api_mining/traces"
    
  # Performance targets
  performance:
    target_overhead_percent: 5.0
    max_overhead_us: 10000  # 10ms per request
```

## Testing

### Unit Tests

All modules include comprehensive unit tests:
- `trace_buffer.rs`: Ring buffer behavior
- `trace_storage.rs`: File I/O operations
- `trace_manager.rs`: Multi-tab coordination

Run tests:
```bash
cd magician
cargo test --package magician --lib api_mining
```

### Integration Tests

**File**: `magician/tests/api_mining_integration_test.rs`

```rust
#[tokio::test]
async fn test_end_to_end_trace_capture() {
    // 1. Start browser automation session
    // 2. Execute actions that trigger XHR/Fetch
    // 3. Verify traces are captured
    // 4. Check trace persistence
    // 5. Validate statistics
}

#[tokio::test]
async fn test_trace_capture_performance() {
    // 1. Execute automation with trace capture
    // 2. Execute same automation without capture
    // 3. Compare execution times
    // 4. Assert <5% overhead
}
```

### Manual Testing

1. **Basic Capture Test**
   ```bash
   # Run Magician with trace capture enabled
   cargo run --bin magician -- --config magician-config.yaml
   
   # Execute a task that triggers network requests
   # E.g., navigate to a web app and perform actions
   
   # Verify traces were written
   ls -lh storage/magician/api_mining/traces/
   ```

2. **Overhead Test**
   ```bash
   # Run automation 10 times WITH trace capture
   for i in {1..10}; do
     time cargo run --bin magician -- task.json
   done
   
   # Disable trace capture in config
   # Run automation 10 times WITHOUT trace capture
   # Compare average execution times
   ```

3. **Capacity Test**
   ```bash
   # Execute long-running automation with many requests
   # Verify ring buffer stays within limits
   # Check no memory leaks
   ```

## Metrics & Observability

### Metrics to Track

1. **Capture Rate**
   - Total requests observed
   - Successful captures
   - Failed captures (with reasons)

2. **Performance**
   - Average overhead per request (μs)
   - Max overhead observed (μs)
   - Total CPU time for capture

3. **Storage**
   - Total traces persisted
   - Total bytes written
   - Disk space used

4. **Buffer Health**
   - Dropped entries (oversized)
   - Dropped entries (buffer full)
   - Current buffer utilization

### Logging

Add structured logging:

```rust
log::info!(
    "Trace capture: requests={}, overhead={:.2}μs, size={}",
    stats.total_requests,
    stats.avg_overhead_us,
    stats.total_bytes_captured
);
```

## Success Criteria

- [x] Ring buffer implementation with 100 entry / 1MB limits
- [x] Trace storage in JSONL format
- [x] Rust-side type definitions and integration points
- [ ] Browser extension CDP integration (see EXTENSION_TRACE_CAPTURE_SPEC.md)
- [ ] End-to-end test capturing traces from real automation
- [ ] Performance validation: <5% overhead
- [ ] 100% capture rate for XHR/Fetch requests

## Next Steps

After Phase 1 completion:

1. **Phase 2: API Mining**
   - Implement clustering engine
   - Build parameterization logic
   - Create API capability registry

2. **Performance Optimization**
   - Profile capture overhead
   - Optimize buffer operations
   - Implement adaptive capture (skip redundant requests)

3. **Security Hardening**
   - Token redaction in traces
   - Encryption at rest
   - Access controls
