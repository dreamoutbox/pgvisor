use std::sync::atomic::{AtomicU64, Ordering};

/// Collects container / host CPU and memory metrics from `/proc`.
pub struct SystemMetricsCollector {
    prev_idle: AtomicU64,
    prev_total: AtomicU64,
}

impl Default for SystemMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMetricsCollector {
    pub fn new() -> Self {
        let (idle, total) = Self::read_proc_stat_raw().unwrap_or((0, 0));
        Self {
            prev_idle: AtomicU64::new(idle),
            prev_total: AtomicU64::new(total),
        }
    }

    fn read_proc_stat_raw() -> Option<(u64, u64)> {
        let contents = std::fs::read_to_string("/proc/stat").ok()?;
        Self::parse_proc_stat(&contents)
    }

    /// Parses the first cpu line from `/proc/stat`.
    pub fn parse_proc_stat(contents: &str) -> Option<(u64, u64)> {
        let first_line = contents.lines().next()?;
        if !first_line.starts_with("cpu ") {
            return None;
        }
        let values: Vec<u64> = first_line
            .split_whitespace()
            .skip(1)
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();
        if values.len() < 4 {
            return None;
        }
        let total: u64 = values.iter().sum();
        let idle = values.get(3).copied().unwrap_or(0) + values.get(4).copied().unwrap_or(0);
        Some((idle, total))
    }

    /// Computes CPU utilization percentage since last check.
    pub fn cpu_percent(&self) -> f32 {
        if let Some((idle, total)) = Self::read_proc_stat_raw() {
            let prev_idle = self.prev_idle.swap(idle, Ordering::Relaxed);
            let prev_total = self.prev_total.swap(total, Ordering::Relaxed);

            let total_delta = total.saturating_sub(prev_total);
            let idle_delta = idle.saturating_sub(prev_idle);

            if total_delta > 0 {
                let active = total_delta.saturating_sub(idle_delta);
                let pct = (active as f64 / total_delta as f64) * 100.0;
                return (pct as f32).clamp(0.0, 100.0);
            }
        }
        0.0
    }

    /// Parses memory info lines from `/proc/meminfo` content.
    pub fn parse_proc_meminfo(contents: &str) -> (u64, u64) {
        let mut total_kb: Option<u64> = None;
        let mut available_kb: Option<u64> = None;
        let mut free_kb: Option<u64> = None;
        let mut buffers_kb: Option<u64> = None;
        let mut cached_kb: Option<u64> = None;

        for line in contents.lines() {
            let mut parts = line.split_whitespace();
            let key = match parts.next() {
                Some(k) => k,
                None => continue,
            };
            let val = match parts.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(v) => v,
                None => continue,
            };

            match key {
                "MemTotal:" => total_kb = Some(val),
                "MemAvailable:" => available_kb = Some(val),
                "MemFree:" => free_kb = Some(val),
                "Buffers:" => buffers_kb = Some(val),
                "Cached:" => cached_kb = Some(val),
                _ => {}
            }
        }

        let total = total_kb.unwrap_or(0) * 1024;
        let avail = if let Some(a) = available_kb {
            a * 1024
        } else {
            (free_kb.unwrap_or(0) + buffers_kb.unwrap_or(0) + cached_kb.unwrap_or(0)) * 1024
        };

        let used = total.saturating_sub(avail);
        (used, total)
    }

    /// Reads memory usage from `/proc/meminfo`.
    /// Returns (used_bytes, total_bytes).
    pub fn memory_usage(&self) -> (u64, u64) {
        let contents = match std::fs::read_to_string("/proc/meminfo") {
            Ok(c) => c,
            Err(_) => return (0, 0),
        };
        Self::parse_proc_meminfo(&contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proc_stat() {
        let mock = "cpu  72842 162 30317 3311198 4564 0 5380 0 0 0\ncpu0 3312 0 1678 164886 269 0 3495 0 0 0\n";
        let (idle, total) = SystemMetricsCollector::parse_proc_stat(mock).expect("parse cpu");
        assert_eq!(idle, 3311198 + 4564);
        assert!(total > idle);
    }

    #[test]
    fn test_parse_proc_meminfo() {
        let mock = "MemTotal:       1000000 kB\nMemFree:          100000 kB\nMemAvailable:    400000 kB\nBuffers:          50000 kB\n";
        let (used, total) = SystemMetricsCollector::parse_proc_meminfo(mock);
        assert_eq!(total, 1000000 * 1024);
        assert_eq!(used, 600000 * 1024);
    }
}
