//! What the host machine can hold, for the local-model memory gate.
//!
//! Models that run inside the decision engine (`laya-onnx`, `laya-mlx`,
//! `kev-onnx`, `kev-mlx`) bind only when the machine has at least
//! `local_min_memory_gb` (default 16, like local generation's
//! `min_memory_gb`) or the entry's own `min_memory_gb`. Below it they stay
//! idle and the route falls through to the next model — a hosted one, or
//! none. HTTP models (`typesafe`, `systemone`) use no memory here and are
//! never gated.
//!
//! Memory is physical RAM, capped on Linux by the process's cgroup limit (a
//! 64 GB host running the engine in an 8 GB container has 8 GB). Unreadable
//! memory is 0 and fails the gate: guessing high would load a model the
//! machine cannot hold. `DECISION_HOST_MEMORY_GB` overrides the reading.

use std::sync::OnceLock;

/// Adapters whose models run inside the engine process.
pub fn runs_in_process(adapter: &str) -> bool {
    matches!(adapter, "laya-onnx" | "laya-mlx" | "kev-onnx" | "kev-mlx")
}

/// Whether a model may bind on a machine with `have_gb`: `Err` names why
/// not. `model_min` (the entry's own bar) replaces `local_min` when set.
pub fn memory_gate(
    adapter: &str,
    model_min: Option<u32>,
    local_min: u32,
    have_gb: u64,
) -> Result<(), String> {
    if !runs_in_process(adapter) {
        return Ok(());
    }
    let need = model_min.unwrap_or(local_min) as u64;
    if have_gb >= need {
        return Ok(());
    }
    Err(if have_gb == 0 {
        format!("this machine's memory could not be read, and {adapter} needs {need} GB")
    } else {
        format!("this machine has {have_gb} GB and {adapter} needs {need} GB")
    })
}

/// The machine's usable memory in GB, read once.
pub fn host_memory_gb() -> u64 {
    static READ: OnceLock<u64> = OnceLock::new();
    *READ.get_or_init(|| {
        if let Some(gb) = std::env::var("DECISION_HOST_MEMORY_GB")
            .ok()
            .and_then(|v| v.trim().parse().ok())
        {
            return gb;
        }
        usable_gb(physical_bytes(), cgroup_limit_bytes())
    })
}

const GIB: f64 = (1u64 << 30) as f64;

/// Physical memory rounded to the nearest GB (Linux's MemTotal excludes
/// kernel-reserved memory, so a 16 GB machine reads ~15.6), capped by a
/// cgroup limit taken as is (a limit is deliberate).
fn usable_gb(physical: Option<u64>, cgroup: Option<u64>) -> u64 {
    let Some(physical) = physical.filter(|&b| b > 0) else {
        return 0;
    };
    let physical_gb = (physical as f64 / GIB).round() as u64;
    match cgroup {
        Some(limit) => physical_gb.min((limit as f64 / GIB).floor() as u64),
        None => physical_gb,
    }
}

#[cfg(target_os = "macos")]
fn physical_bytes() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: sysctlbyname writes at most `size` bytes into `bytes`.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0).then_some(bytes)
}

#[cfg(target_os = "linux")]
fn physical_bytes() -> Option<u64> {
    meminfo_total_bytes(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn physical_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn cgroup_limit_bytes() -> Option<u64> {
    // cgroup v2, then v1. Inside a container /sys/fs/cgroup is its own
    // cgroup; on a bare host the root has no memory.max.
    [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ]
    .iter()
    .find_map(|path| cgroup_limit(&std::fs::read_to_string(path).ok()?))
}

#[cfg(not(target_os = "linux"))]
fn cgroup_limit_bytes() -> Option<u64> {
    None
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// A cgroup memory limit: `max`, or v1's page-rounded "unlimited", is none.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit(text: &str) -> Option<u64> {
    let bytes: u64 = text.trim().parse().ok()?;
    (bytes < 1 << 60).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    #[test]
    fn only_in_process_models_are_gated() {
        assert!(memory_gate("typesafe", None, 16, 4).is_ok());
        assert!(memory_gate("systemone", None, 16, 0).is_ok());
        assert!(memory_gate("kev-mlx", None, 16, 16).is_ok());
        let refused = memory_gate("laya-onnx", None, 16, 8).unwrap_err();
        assert!(
            refused.contains("8 GB") && refused.contains("16 GB"),
            "{refused}"
        );
        assert!(
            memory_gate("kev-mlx", Some(24), 16, 18).is_err(),
            "the entry's own bar wins"
        );
        assert!(memory_gate("kev-mlx", Some(8), 16, 12).is_ok());
        assert!(memory_gate("laya-mlx", None, 16, 0)
            .unwrap_err()
            .contains("could not be read"));
    }

    #[test]
    fn usable_memory_is_ram_capped_by_the_container() {
        // A "16 GB" Linux machine reports ~15.6 GiB of MemTotal.
        assert_eq!(usable_gb(Some(16_384_000 * 1024), None), 16);
        assert_eq!(usable_gb(Some(64 * GB), Some(8 * GB)), 8);
        assert_eq!(usable_gb(Some(16 * GB), Some(64 * GB)), 16);
        assert_eq!(usable_gb(None, None), 0);
        assert_eq!(usable_gb(Some(0), None), 0);
    }

    #[test]
    fn linux_readings_parse() {
        let meminfo = "MemTotal:       16384000 kB\nMemFree:  1 kB\n";
        assert_eq!(meminfo_total_bytes(meminfo), Some(16_384_000 * 1024));
        assert_eq!(cgroup_limit("max\n"), None);
        assert_eq!(cgroup_limit("9223372036854771712\n"), None);
        assert_eq!(cgroup_limit("8589934592\n"), Some(8 * GB));
    }
}
