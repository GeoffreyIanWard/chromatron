//! Peak resident set size, for the memory gate.
//!
//! `bench/memory-budget.md` budgets **8 GB for the process** on min-spec, and
//! that is a claim about resident memory — pages the OS has actually backed —
//! not about bytes the allocator handed out. Those differ enough to matter:
//! allocator accounting misses page-level behaviour, memory-mapped regions, and
//! anything `bevy_ecs` or the task pool maps directly, and it counts freed-but-
//! unreturned arena space that the OS may have reclaimed.
//!
//! # Linux is the authority, deliberately
//!
//! `/proc/self/status`'s `VmHWM` is the process's *high water mark* — peak RSS
//! since it started — which is exactly what a budget is about, and it is
//! readable without a dependency, without `unsafe`, and without spawning
//! anything.
//!
//! macOS and Windows have equivalents (`task_info`, `GetProcessMemoryInfo`) but
//! both need FFI, and neither runs the gate: `bench/baselines.md` names Linux as
//! the reference hardware and the CI gates job runs on `ubuntu-latest`. Rather
//! than approximate the number on platforms that do not gate it, this reports
//! [`None`] and the benchmark says plainly that it measured nothing. A gate that
//! silently reports a wrong number on a developer laptop is worse than one that
//! admits it cannot measure there.

/// Peak resident set size in bytes, if this platform can report it.
///
/// Returns `None` on platforms without a supported probe — see the module docs
/// for why that is a deliberate gap rather than an oversight.
#[cfg(target_os = "linux")]
pub fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vm_hwm(&status)
}

/// Peak resident set size in bytes, if this platform can report it.
#[cfg(not(target_os = "linux"))]
pub fn peak_rss_bytes() -> Option<u64> {
    None
}

/// Extracts `VmHWM` from `/proc/self/status`, in bytes.
///
/// The field is reported in kibibytes: `VmHWM:  123456 kB`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_vm_hwm(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kibibytes| kibibytes.parse::<u64>().ok())
        .map(|kibibytes| kibibytes * 1024)
}

/// Whether this platform can report peak RSS.
pub fn is_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Bytes rendered as gibibytes, for messages.
pub fn as_gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_high_water_mark_field() {
        let status =
            "Name:\tchromatron\nVmPeak:\t 9999999 kB\nVmHWM:\t  123456 kB\nVmRSS:\t   1000 kB\n";
        assert_eq!(parse_vm_hwm(status), Some(123_456 * 1024));
    }

    #[test]
    fn reads_hwm_rather_than_current_rss() {
        // VmRSS is current and VmPeak is peak *virtual* size; neither is the
        // number the budget is about. Getting this wrong would under- or
        // over-report by a wide margin.
        let status = "VmPeak:\t 8000000 kB\nVmRSS:\t   50000 kB\nVmHWM:\t  200000 kB\n";
        assert_eq!(parse_vm_hwm(status), Some(200_000 * 1024));
    }

    #[test]
    fn a_missing_field_reports_nothing_rather_than_zero() {
        // Zero would look like a spectacular pass.
        assert_eq!(parse_vm_hwm("Name:\tchromatron\nVmRSS:\t 1000 kB\n"), None);
    }

    #[test]
    fn gibibyte_conversion_is_binary_not_decimal() {
        assert!((as_gib(8 * 1024 * 1024 * 1024) - 8.0).abs() < f64::EPSILON);
    }
}
