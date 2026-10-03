//! Process memory: a platform sampler and a pure tracker that decides what to
//! log.
//!
//! [`sample`] reads the operating system's own accounting of this process:
//! on macOS the physical footprint (`task_vm_info.phys_footprint`, Activity
//! Monitor's Memory column, which counts compressed and swapped-out dirty
//! pages that resident size omits) and its lifetime peak; on Windows the
//! private bytes (`PrivateUsage`) and peak private commit
//! (`PeakPagefileUsage`). Other platforms, and a failed read, give `None`.
//!
//! [`MemoryTracker`] performs no clock reads, I/O, or logging. The caller
//! supplies each sample with the times it was taken and emits the returned
//! [`MemoryLog`] through [`emit`] on [`LOG_TARGET`].

use std::time::{Duration, Instant, SystemTime};

/// The tracing target memory events log on; configurable as `memory` in
/// `[log]`.
pub const LOG_TARGET: &str = "geode::memory";

const MIB: u64 = 1024 * 1024;

/// An absolute peak rise at or above this logs at info.
pub const PEAK_STEP_BYTES: u64 = 256 * MIB;

/// A peak rise of at least this fraction (one quarter, 25%) of the last
/// logged peak logs at info.
pub const PEAK_STEP_DIVISOR: u64 = 4;

/// The shortest interval between periodic debug events.
pub const DEBUG_INTERVAL: Duration = Duration::from_secs(60);

/// Whether [`sample`] can read anything on this platform.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

/// What [`sample`] measures, as the Performance page names it.
pub const MEASURE: &str = if cfg!(target_os = "macos") {
    "physical footprint"
} else if cfg!(windows) {
    "private bytes"
} else {
    "not measured on this platform"
};

/// One operating-system reading of this process's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub current_bytes: u64,
    /// The operating system's lifetime peak of the same measure.
    pub peak_bytes: u64,
}

/// The tracker's state after a sample, as diagnostics shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReading {
    pub current_bytes: u64,
    /// Never decreases, and never reads below a current value seen.
    pub peak_bytes: u64,
    /// The wall time of the sample that first saw `peak_bytes`. The first
    /// sample's peak may predate it: the operating system's peak covers the
    /// whole process lifetime.
    pub peak_at: SystemTime,
}

/// Why a sample logs at info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeakLog {
    /// The first sample, logged once as the baseline.
    Baseline,
    /// The peak rose past a threshold above the last logged peak.
    Rose { from_bytes: u64 },
}

/// What one sample asks the caller to log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryLog {
    pub peak: Option<PeakLog>,
    /// Log current and peak at debug.
    pub periodic: bool,
}

/// Current, peak and peak time across samples, with the logging cadence.
#[derive(Debug, Clone, Default)]
pub struct MemoryTracker {
    reading: Option<MemoryReading>,
    logged_peak: u64,
    last_debug: Option<Instant>,
}

impl MemoryTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The state after the last sample, `None` before the first.
    pub fn reading(&self) -> Option<&MemoryReading> {
        self.reading.as_ref()
    }

    /// Fold in one sample taken at monotonic `now` and wall time `wall`.
    ///
    /// The first sample logs its baseline at info and starts the debug
    /// cadence, so its first periodic event follows [`DEBUG_INTERVAL`] later.
    /// Later samples log at info when the peak rises at least
    /// [`PEAK_STEP_BYTES`] or a quarter above the last *logged* peak, so slow
    /// growth logs once it accumulates rather than never.
    pub fn observe(&mut self, sample: ProcessMemory, now: Instant, wall: SystemTime) -> MemoryLog {
        let seen = sample.peak_bytes.max(sample.current_bytes);
        let Some(prev) = self.reading else {
            self.reading = Some(MemoryReading {
                current_bytes: sample.current_bytes,
                peak_bytes: seen,
                peak_at: wall,
            });
            self.logged_peak = seen;
            self.last_debug = Some(now);
            return MemoryLog {
                peak: Some(PeakLog::Baseline),
                periodic: false,
            };
        };
        let (peak_bytes, peak_at) = if seen > prev.peak_bytes {
            (seen, wall)
        } else {
            (prev.peak_bytes, prev.peak_at)
        };
        self.reading = Some(MemoryReading {
            current_bytes: sample.current_bytes,
            peak_bytes,
            peak_at,
        });
        let peak = peak_rose_enough(self.logged_peak, peak_bytes).then(|| {
            let from_bytes = self.logged_peak;
            self.logged_peak = peak_bytes;
            PeakLog::Rose { from_bytes }
        });
        let periodic = self
            .last_debug
            .is_none_or(|last| now.saturating_duration_since(last) >= DEBUG_INTERVAL);
        if periodic {
            self.last_debug = Some(now);
        }
        MemoryLog { peak, periodic }
    }
}

/// Whether `peak` is far enough above `logged` to log at info: a rise of at
/// least [`PEAK_STEP_BYTES`], or of at least `logged / PEAK_STEP_DIVISOR`.
fn peak_rose_enough(logged: u64, peak: u64) -> bool {
    let rise = peak.saturating_sub(logged);
    rise > 0 && (rise >= PEAK_STEP_BYTES || rise.saturating_mul(PEAK_STEP_DIVISOR) >= logged)
}

/// Emit `log` for `reading` on [`LOG_TARGET`].
pub fn emit(log: MemoryLog, reading: &MemoryReading) {
    let current = format_bytes(reading.current_bytes);
    let peak = format_bytes(reading.peak_bytes);
    match log.peak {
        Some(PeakLog::Baseline) => tracing::info!(
            target: LOG_TARGET,
            "process memory ({MEASURE}): {current}, peak {peak}"
        ),
        Some(PeakLog::Rose { from_bytes }) => tracing::info!(
            target: LOG_TARGET,
            "process memory peak rose to {peak} from {}; now {current}",
            format_bytes(from_bytes)
        ),
        None => {}
    }
    if log.periodic {
        tracing::debug!(target: LOG_TARGET, "process memory: {current}, peak {peak}");
    }
}

/// A plain binary-unit byte count with at most one decimal place — a
/// diagnostics readout, not a figure a trader reads regularly, so no
/// humanize crate. Diagnostics compares readings at this granularity, so
/// a change that does not alter this text does not repaint.
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.1}KB", b / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Read this process's memory from the operating system, `None` when the
/// platform is unsupported or the read fails.
pub fn sample() -> Option<ProcessMemory> {
    platform::sample()
}

#[cfg(target_os = "macos")]
mod platform {
    use super::ProcessMemory;
    use mach2::kern_return::KERN_SUCCESS;
    use mach2::message::mach_msg_type_number_t;
    use mach2::task::task_info;
    use mach2::task_info::{TASK_VM_INFO, task_info_t, task_vm_info};
    use mach2::traps::mach_task_self;
    use mach2::vm_types::natural_t;
    use std::mem::{offset_of, size_of};

    /// The `natural_t` count covering `task_vm_info` through
    /// `ledger_phys_footprint_peak` (`TASK_VM_INFO_REV1_COUNT` in
    /// <mach/task_info.h>). A kernel answers with the count it filled; one
    /// short of this has no peak field to read.
    const PEAK_COUNT: usize = (offset_of!(task_vm_info, ledger_phys_footprint_peak)
        + size_of::<i64>())
        / size_of::<natural_t>();

    pub(super) fn sample() -> Option<ProcessMemory> {
        let mut info = task_vm_info::default();
        let mut count =
            (size_of::<task_vm_info>() / size_of::<natural_t>()) as mach_msg_type_number_t;
        // SAFETY: `info` is a writable `task_vm_info` and `count` holds its
        // size in `natural_t` units, so the kernel writes within it.
        let status = unsafe {
            task_info(
                mach_task_self(),
                TASK_VM_INFO,
                (&raw mut info) as task_info_t,
                &mut count,
            )
        };
        if status != KERN_SUCCESS || (count as usize) < PEAK_COUNT {
            return None;
        }
        // Copies out of the packed struct; no reference to a field is taken.
        let current_bytes = info.phys_footprint;
        let peak = info.ledger_phys_footprint_peak;
        Some(ProcessMemory {
            current_bytes,
            peak_bytes: u64::try_from(peak).unwrap_or(0),
        })
    }
}

#[cfg(windows)]
mod platform {
    use super::ProcessMemory;
    use std::mem::size_of;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub(super) fn sample() -> Option<ProcessMemory> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo-handle needs no closing; `counters` is writable
        // and `cb` gives its size, which the call accepts for the EX layout.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(ProcessMemory {
            current_bytes: counters.PrivateUsage as u64,
            peak_bytes: counters.PeakPagefileUsage as u64,
        })
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::ProcessMemory;

    pub(super) fn sample() -> Option<ProcessMemory> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(current_mib: u64, peak_mib: u64) -> ProcessMemory {
        ProcessMemory {
            current_bytes: current_mib * MIB,
            peak_bytes: peak_mib * MIB,
        }
    }

    struct Clock {
        start: Instant,
        wall: SystemTime,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                start: Instant::now(),
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            }
        }
        fn at(&self, secs: u64) -> (Instant, SystemTime) {
            let d = Duration::from_secs(secs);
            (self.start + d, self.wall + d)
        }
    }

    fn observe(t: &mut MemoryTracker, c: &Clock, secs: u64, m: ProcessMemory) -> MemoryLog {
        let (now, wall) = c.at(secs);
        t.observe(m, now, wall)
    }

    #[test]
    fn the_first_sample_logs_the_baseline_once_and_no_debug() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        assert!(t.reading().is_none());
        let log = observe(&mut t, &c, 0, mem(1000, 1200));
        assert_eq!(log.peak, Some(PeakLog::Baseline));
        assert!(!log.periodic, "the baseline is the first record");
        let r = t.reading().unwrap();
        assert_eq!((r.current_bytes, r.peak_bytes), (1000 * MIB, 1200 * MIB));
        assert_eq!(r.peak_at, c.at(0).1);
        let again = observe(&mut t, &c, 1, mem(1000, 1200));
        assert_eq!(again, MemoryLog::default(), "the baseline logs once");
    }

    #[test]
    fn a_peak_rise_of_256_mib_logs_at_info_even_below_a_quarter() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        // 256 MiB over a 4000 MiB peak is 6.4%: only the absolute step fires.
        observe(&mut t, &c, 0, mem(4000, 4000));
        assert_eq!(observe(&mut t, &c, 1, mem(4255, 4255)).peak, None);
        assert_eq!(
            observe(&mut t, &c, 2, mem(4256, 4256)).peak,
            Some(PeakLog::Rose {
                from_bytes: 4000 * MIB
            })
        );
    }

    #[test]
    fn a_peak_rise_of_a_quarter_logs_at_info_even_below_256_mib() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        // 25% over 400 MiB is 100 MiB, well under the absolute step.
        observe(&mut t, &c, 0, mem(400, 400));
        assert_eq!(observe(&mut t, &c, 1, mem(499, 499)).peak, None);
        assert_eq!(
            observe(&mut t, &c, 2, mem(500, 500)).peak,
            Some(PeakLog::Rose {
                from_bytes: 400 * MIB
            })
        );
    }

    #[test]
    fn the_threshold_is_measured_from_the_last_logged_peak() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        observe(&mut t, &c, 0, mem(400, 400));
        // Small rises accumulate against the logged 400, not the last sample.
        for (s, peak) in [(1, 450), (2, 480), (3, 499)] {
            assert_eq!(observe(&mut t, &c, s, mem(peak, peak)).peak, None);
        }
        assert!(observe(&mut t, &c, 4, mem(500, 500)).peak.is_some());
        // The next step measures from 500: 624 is under 25%, 625 is not.
        assert_eq!(observe(&mut t, &c, 5, mem(624, 624)).peak, None);
        assert_eq!(
            observe(&mut t, &c, 6, mem(625, 625)).peak,
            Some(PeakLog::Rose {
                from_bytes: 500 * MIB
            })
        );
    }

    #[test]
    fn the_peak_never_falls_and_keeps_the_time_it_was_reached() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        observe(&mut t, &c, 0, mem(100, 100));
        observe(&mut t, &c, 5, mem(300, 300));
        // A falling current and a smaller reported peak leave the peak.
        observe(&mut t, &c, 9, mem(200, 250));
        let r = t.reading().unwrap();
        assert_eq!(r.current_bytes, 200 * MIB);
        assert_eq!(r.peak_bytes, 300 * MIB);
        assert_eq!(r.peak_at, c.at(5).1, "the sample that first saw 300");
        // A current above the reported peak raises the peak.
        observe(&mut t, &c, 12, mem(350, 300));
        let r = t.reading().unwrap();
        assert_eq!((r.peak_bytes, r.peak_at), (350 * MIB, c.at(12).1));
    }

    #[test]
    fn debug_logs_at_most_once_per_interval() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        observe(&mut t, &c, 0, mem(100, 100));
        assert!(!observe(&mut t, &c, 59, mem(100, 100)).periodic);
        assert!(observe(&mut t, &c, 60, mem(100, 100)).periodic);
        assert!(!observe(&mut t, &c, 61, mem(100, 100)).periodic);
        assert!(!observe(&mut t, &c, 119, mem(100, 100)).periodic);
        assert!(observe(&mut t, &c, 120, mem(100, 100)).periodic);
    }

    #[test]
    fn an_unchanged_peak_logs_nothing_at_info() {
        let c = Clock::new();
        let mut t = MemoryTracker::new();
        observe(&mut t, &c, 0, mem(0, 0));
        // A zero baseline must not make every later sample a 25% rise.
        assert_eq!(observe(&mut t, &c, 1, mem(0, 0)).peak, None);
        assert!(observe(&mut t, &c, 2, mem(1, 1)).peak.is_some());
    }

    /// `[log] memory = "debug"` and the Levels popover both need the target
    /// in the known list; an unlisted target is dropped at reload.
    #[test]
    fn the_log_target_is_a_known_configurable_target() {
        assert!(geode_core::log::TARGETS.contains(&LOG_TARGET));
    }

    #[test]
    fn format_bytes_uses_binary_units_with_one_decimal() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1536), "1.5KB");
        assert_eq!(format_bytes(256 * MIB), "256.0MB");
        assert_eq!(format_bytes(7 * 1024 * MIB + 102 * MIB), "7.1GB");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn the_sampler_reads_this_process() {
        let m = sample().expect("a supported platform reads its own process");
        assert!(m.current_bytes > 0);
        assert!(m.peak_bytes >= m.current_bytes, "{m:?}");
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn the_sampler_reads_nothing_on_an_unsupported_platform() {
        assert_eq!(sample(), None);
    }
}
