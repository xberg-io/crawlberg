//! Real-time resource monitoring for benchmark execution.
//!
//! Spawns a background tokio task that samples RSS memory, virtual memory,
//! and CPU usage at configurable intervals. Supports the entire process tree
//! (parent + child processes spawned by browser mode).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::Mutex;

use crate::stats::{percentile_r7, sanitize_f64};

/// A single resource usage sample.
#[derive(Debug, Clone, Copy)]
pub struct ResourceSample {
    /// Resident set size of the process tree in bytes.
    pub memory_bytes: u64,
    /// Virtual memory of the process tree in bytes.
    pub vm_bytes: u64,
    /// CPU utilization of the process tree as a percentage (0–100), normalised per-core.
    pub cpu_percent: f64,
    /// Milliseconds elapsed since monitoring started.
    pub timestamp_ms: u64,
}

/// Aggregated resource metrics from a monitoring session.
#[derive(Debug, Clone)]
pub struct ResourceMetrics {
    /// Peak RSS observed across all samples.
    pub peak_memory_bytes: u64,
    /// 50th-percentile RSS across all samples.
    pub p50_memory_bytes: u64,
    /// 95th-percentile RSS across all samples.
    pub p95_memory_bytes: u64,
    /// 99th-percentile RSS across all samples.
    pub p99_memory_bytes: u64,
    /// Mean CPU utilisation across all samples, in percent.
    pub avg_cpu_percent: f64,
    /// Peak CPU utilisation observed across all samples, in percent.
    pub peak_cpu_percent: f64,
    /// Number of samples collected.
    pub sample_count: usize,
    /// RSS captured before the first sample (process baseline).
    pub baseline_memory_bytes: u64,
}

/// Background resource monitor.
///
/// Create with [`ResourceMonitor::new`], call [`ResourceMonitor::start`] to begin
/// sampling, [`ResourceMonitor::stop`] to end, and [`ResourceMonitor::metrics`] to
/// compute aggregated results. Call [`ResourceMonitor::reset`] between benchmark
/// iterations to clear accumulated samples.
pub struct ResourceMonitor {
    samples: Arc<Mutex<Vec<ResourceSample>>>,
    running: Arc<AtomicBool>,
    pid: Pid,
    baseline_memory: u64,
}

impl ResourceMonitor {
    /// Create a new monitor bound to the current process.
    pub fn new() -> Self {
        let pid = sysinfo::get_current_pid().unwrap_or_else(|_| Pid::from_u32(std::process::id()));
        Self {
            samples: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            pid,
            baseline_memory: 0,
        }
    }

    /// Start background sampling at the given interval.
    ///
    /// Captures a baseline RSS reading before spawning the background task.
    /// Calling `start` on an already-running monitor is a no-op.
    ///
    /// The background task runs until [`ResourceMonitor::stop`] is called.
    pub async fn start(&mut self, sample_interval: Duration) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        let samples = Arc::clone(&self.samples);
        let running = Arc::clone(&self.running);
        let pid = self.pid;
        let refresh_kind = ProcessRefreshKind::nothing().with_memory().with_cpu();

        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, false, refresh_kind);
        self.baseline_memory = process_tree_memory(pid, &sys);

        tokio::time::sleep(sample_interval).await;

        tokio::spawn(async move {
            let mut system = System::new();
            let start = Instant::now();

            system.refresh_processes_specifics(ProcessesToUpdate::All, false, refresh_kind);
            tokio::time::sleep(sample_interval).await;

            let num_cpus = num_cpus::get().max(1) as f64;

            while running.load(Ordering::Relaxed) {
                system.refresh_processes_specifics(ProcessesToUpdate::All, false, refresh_kind);

                let elapsed = start.elapsed();
                let memory = process_tree_memory(pid, &system);
                let vm = process_tree_vm(pid, &system);
                let cpu = process_tree_cpu(pid, &system) / num_cpus;

                let sample = ResourceSample {
                    memory_bytes: memory,
                    vm_bytes: vm,
                    cpu_percent: cpu,
                    timestamp_ms: elapsed.as_millis() as u64,
                };

                samples.lock().await.push(sample);
                tokio::time::sleep(sample_interval).await;
            }
        });
    }

    /// Signal the background task to stop after its current sleep completes.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Compute aggregated metrics from the samples collected so far.
    ///
    /// Returns zeroed metrics when no samples have been collected yet.
    pub async fn metrics(&self) -> ResourceMetrics {
        let samples = self.samples.lock().await;

        if samples.is_empty() {
            return ResourceMetrics {
                peak_memory_bytes: 0,
                p50_memory_bytes: 0,
                p95_memory_bytes: 0,
                p99_memory_bytes: 0,
                avg_cpu_percent: 0.0,
                peak_cpu_percent: 0.0,
                sample_count: 0,
                baseline_memory_bytes: self.baseline_memory,
            };
        }

        let mut memory_values: Vec<f64> = samples.iter().map(|s| s.memory_bytes as f64).collect();
        let cpu_values: Vec<f64> = samples.iter().map(|s| s.cpu_percent).collect();

        let peak_memory = samples.iter().map(|s| s.memory_bytes).max().unwrap_or(0);
        let avg_cpu = cpu_values.iter().sum::<f64>() / cpu_values.len() as f64;
        let peak_cpu = cpu_values.iter().cloned().fold(0.0_f64, f64::max);

        let p50_mem = percentile_r7(&mut memory_values, 0.5).unwrap_or(0.0) as u64;
        let p95_mem = percentile_r7(&mut memory_values, 0.95).unwrap_or(0.0) as u64;
        let p99_mem = percentile_r7(&mut memory_values, 0.99).unwrap_or(0.0) as u64;

        ResourceMetrics {
            peak_memory_bytes: peak_memory,
            p50_memory_bytes: p50_mem,
            p95_memory_bytes: p95_mem,
            p99_memory_bytes: p99_mem,
            avg_cpu_percent: sanitize_f64(avg_cpu),
            peak_cpu_percent: sanitize_f64(peak_cpu),
            sample_count: samples.len(),
            baseline_memory_bytes: self.baseline_memory,
        }
    }

    /// Clear all accumulated samples for a new monitoring session.
    ///
    /// Does not stop the background task — call [`ResourceMonitor::stop`] first
    /// if sampling should also be halted.
    pub async fn reset(&self) {
        self.samples.lock().await.clear();
    }
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Capture a point-in-time resource snapshot of the current process tree.
///
/// Used for before/after measurements around individual scrape operations so that
/// each [`crate::types::IterationResult`] carries its own memory footprint rather
/// than relying solely on the global background monitor.
///
/// The `timestamp_ms` field of the returned [`ResourceSample`] is set to `0`
/// because it is not meaningful for a point-in-time snapshot.
pub fn snapshot_resources() -> ResourceSample {
    let pid = sysinfo::get_current_pid().unwrap_or_else(|_| Pid::from_u32(std::process::id()));
    let mut sys = System::new();
    let refresh_kind = ProcessRefreshKind::nothing().with_memory().with_cpu();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, false, refresh_kind);

    let memory = process_tree_memory(pid, &sys);
    let vm = process_tree_vm(pid, &sys);
    let cpu = process_tree_cpu(pid, &sys) / num_cpus::get().max(1) as f64;

    ResourceSample {
        memory_bytes: memory,
        vm_bytes: vm,
        cpu_percent: cpu,
        timestamp_ms: 0,
    }
}

/// Sum RSS memory across a process and all its children (recursive).
pub(crate) fn process_tree_memory(pid: Pid, system: &System) -> u64 {
    let Some(proc) = system.process(pid) else {
        return 0;
    };
    let mut total = proc.memory();
    for (child_pid, child_proc) in system.processes() {
        if child_proc.parent() == Some(pid) {
            total += process_tree_memory(*child_pid, system);
        }
    }
    total
}

/// Sum virtual memory across a process and all its children (recursive).
pub(crate) fn process_tree_vm(pid: Pid, system: &System) -> u64 {
    let Some(proc) = system.process(pid) else {
        return 0;
    };
    let mut total = proc.virtual_memory();
    for (child_pid, child_proc) in system.processes() {
        if child_proc.parent() == Some(pid) {
            total += process_tree_vm(*child_pid, system);
        }
    }
    total
}

/// Sum CPU usage across a process and all its children (recursive).
///
/// The returned value is the raw sum of per-core percentages — divide by
/// `num_cpus` at the call site to obtain a normalised figure.
pub(crate) fn process_tree_cpu(pid: Pid, system: &System) -> f64 {
    let Some(proc) = system.process(pid) else {
        return 0.0;
    };
    let mut total = proc.cpu_usage() as f64;
    for (child_pid, child_proc) in system.processes() {
        if child_proc.parent() == Some(pid) {
            total += process_tree_cpu(*child_pid, system);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_monitor_new_does_not_panic() {
        let monitor = ResourceMonitor::new();
        monitor.stop();
    }

    #[tokio::test]
    async fn metrics_empty_returns_zeroed() {
        let monitor = ResourceMonitor::new();
        let metrics = monitor.metrics().await;
        assert_eq!(metrics.sample_count, 0);
        assert_eq!(metrics.peak_memory_bytes, 0);
        assert_eq!(metrics.p50_memory_bytes, 0);
        assert_eq!(metrics.p95_memory_bytes, 0);
        assert_eq!(metrics.p99_memory_bytes, 0);
        assert_eq!(metrics.avg_cpu_percent, 0.0);
        assert_eq!(metrics.peak_cpu_percent, 0.0);
    }

    #[tokio::test]
    async fn reset_clears_samples() {
        let monitor = ResourceMonitor::new();
        monitor.samples.lock().await.push(ResourceSample {
            memory_bytes: 1024,
            vm_bytes: 2048,
            cpu_percent: 5.0,
            timestamp_ms: 10,
        });
        assert_eq!(monitor.samples.lock().await.len(), 1);
        monitor.reset().await;
        assert_eq!(monitor.samples.lock().await.len(), 0);
    }

    #[tokio::test]
    async fn metrics_from_known_samples() {
        let monitor = ResourceMonitor::new();
        {
            let mut samples = monitor.samples.lock().await;
            for i in 1u64..=5 {
                samples.push(ResourceSample {
                    memory_bytes: i * 1000,
                    vm_bytes: i * 2000,
                    cpu_percent: i as f64 * 10.0,
                    timestamp_ms: i * 100,
                });
            }
        }

        let metrics = monitor.metrics().await;
        assert_eq!(metrics.sample_count, 5);
        assert_eq!(metrics.peak_memory_bytes, 5000);
        assert_eq!(metrics.p50_memory_bytes, 3000);
        assert!((metrics.avg_cpu_percent - 30.0).abs() < 1e-9);
        assert!((metrics.peak_cpu_percent - 50.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn start_stop_collects_samples() {
        let mut monitor = ResourceMonitor::new();
        monitor.start(Duration::from_millis(50)).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        monitor.stop();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let metrics = monitor.metrics().await;
        assert!(
            metrics.sample_count >= 2,
            "expected >=2 samples, got {}",
            metrics.sample_count
        );
    }
}
