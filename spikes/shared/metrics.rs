use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct FrameMetrics {
    process_started: Instant,
    previous_frame: Option<Instant>,
    samples_ms: VecDeque<f64>,
    frames: u64,
    first_frame_ms: Option<f64>,
    last_report: Instant,
    peak_rss_bytes: u64,
}

impl FrameMetrics {
    pub fn new(process_started: Instant) -> Self {
        Self {
            process_started,
            previous_frame: None,
            samples_ms: VecDeque::with_capacity(600),
            frames: 0,
            first_frame_ms: None,
            last_report: Instant::now(),
            peak_rss_bytes: 0,
        }
    }

    pub fn tick(&mut self, renderer: &str) {
        let now = Instant::now();
        if let Some(previous) = self.previous_frame.replace(now) {
            self.samples_ms
                .push_back(now.duration_since(previous).as_secs_f64() * 1_000.0);
            if self.samples_ms.len() > 600 {
                self.samples_ms.pop_front();
            }
        }

        self.frames += 1;
        let rss = working_set_bytes().unwrap_or(0);
        self.peak_rss_bytes = self.peak_rss_bytes.max(rss);

        if self.first_frame_ms.is_none() {
            let elapsed = self.process_started.elapsed().as_secs_f64() * 1_000.0;
            self.first_frame_ms = Some(elapsed);
            println!(
                "MARKDOWN_SPIKE_METRIC renderer={renderer} event=first_frame elapsed_ms={elapsed:.2} rss_mib={:.2}",
                mib(rss)
            );
        }

        if now.duration_since(self.last_report) >= Duration::from_secs(1) {
            self.last_report = now;
            println!(
                "MARKDOWN_SPIKE_METRIC renderer={renderer} event=sample callbacks={} rss_mib={:.2} peak_rss_mib={:.2} p95_callback_interval_ms={:.2}",
                self.frames,
                mib(rss),
                mib(self.peak_rss_bytes),
                self.p95_callback_interval_ms()
            );
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.process_started.elapsed()
    }

    pub fn summary(&self) -> String {
        format!(
            "first {:.1} ms  |  RSS {:.1} MiB  |  peak {:.1} MiB  |  callback p95 {:.1} ms  |  {} callbacks  |  {:.1} s",
            self.first_frame_ms.unwrap_or_default(),
            mib(working_set_bytes().unwrap_or(0)),
            mib(self.peak_rss_bytes),
            self.p95_callback_interval_ms(),
            self.frames,
            self.elapsed().as_secs_f32()
        )
    }

    fn p95_callback_interval_ms(&self) -> f64 {
        if self.samples_ms.is_empty() {
            return 0.0;
        }
        let mut samples: Vec<_> = self.samples_ms.iter().copied().collect();
        samples.sort_by(f64::total_cmp);
        samples[((samples.len() - 1) as f64 * 0.95).round() as usize]
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

#[allow(dead_code)]
pub fn current_rss_mib() -> f64 {
    mib(working_set_bytes().unwrap_or(0))
}

#[cfg(target_os = "windows")]
fn working_set_bytes() -> Option<u64> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };
    (ok != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(target_os = "linux")]
fn working_set_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(kib * 1_024)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn working_set_bytes() -> Option<u64> {
    None
}
