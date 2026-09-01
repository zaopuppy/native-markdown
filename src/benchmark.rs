use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::{App, WeakEntity, Window};

use crate::app::NativeMarkdownApp;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkScenario {
    Idle,
    ViewModes,
    Zoom,
    ZoomSource,
    ZoomSplit,
    Reopen,
    Scroll,
    ImageRelease,
}

impl BenchmarkScenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "idle" => Ok(Self::Idle),
            "view-modes" => Ok(Self::ViewModes),
            "zoom" => Ok(Self::Zoom),
            "zoom-source" => Ok(Self::ZoomSource),
            "zoom-split" => Ok(Self::ZoomSplit),
            "reopen" => Ok(Self::Reopen),
            "scroll" => Ok(Self::Scroll),
            "image-release" => Ok(Self::ImageRelease),
            _ => Err(format!(
                "invalid NATIVE_MARKDOWN_BENCHMARK value {value:?}; expected idle, view-modes, zoom, zoom-source, zoom-split, reopen, scroll, or image-release"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::ViewModes => "view-modes",
            Self::Zoom => "zoom",
            Self::ZoomSource => "zoom-source",
            Self::ZoomSplit => "zoom-split",
            Self::Reopen => "reopen",
            Self::Scroll => "scroll",
            Self::ImageRelease => "image-release",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BenchmarkConfig {
    pub scenario: BenchmarkScenario,
    pub duration: Duration,
    pub warmup: Duration,
    pub step_interval: Duration,
    pub max_steps: Option<u64>,
    pub secondary_document: Option<PathBuf>,
    pub switch_step: u64,
    pub max_private_working_set_bytes: u64,
    pub max_private_bytes: u64,
    pub max_growth_bytes: u64,
}

impl BenchmarkConfig {
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let Some(scenario) = lookup("NATIVE_MARKDOWN_BENCHMARK") else {
            return Ok(None);
        };

        let scenario = BenchmarkScenario::parse(&scenario)?;
        let secondary_document = lookup("NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        if scenario == BenchmarkScenario::ImageRelease && secondary_document.is_none() {
            return Err(
                "NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT is required for image-release"
                    .to_owned(),
            );
        }

        Ok(Some(Self {
            scenario,
            duration: Duration::from_secs(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_SECONDS",
                10,
                1,
                300,
            )?),
            warmup: Duration::from_millis(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_WARMUP_MS",
                1_500,
                0,
                30_000,
            )?),
            step_interval: Duration::from_millis(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_STEP_MS",
                50,
                10,
                10_000,
            )?),
            max_steps: parse_optional_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_STEPS",
                1,
                1_000_000,
            )?,
            secondary_document,
            switch_step: parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_SWITCH_STEP",
                100,
                1,
                1_000_000,
            )?,
            max_private_working_set_bytes: mib(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_WS_MIB",
                160,
                1,
                65_536,
            )?),
            max_private_bytes: mib(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_BYTES_MIB",
                160,
                1,
                65_536,
            )?),
            max_growth_bytes: mib(parse_bounded(
                &mut lookup,
                "NATIVE_MARKDOWN_BENCHMARK_MAX_GROWTH_MIB",
                80,
                0,
                65_536,
            )?),
        }))
    }

    #[cfg(test)]
    fn for_test(
        scenario: BenchmarkScenario,
        max_private_working_set_mib: u64,
        max_private_bytes_mib: u64,
        max_growth_mib: u64,
    ) -> Self {
        Self {
            scenario,
            duration: Duration::from_secs(1),
            warmup: Duration::ZERO,
            step_interval: Duration::from_millis(10),
            max_steps: None,
            secondary_document: None,
            switch_step: 100,
            max_private_working_set_bytes: mib(max_private_working_set_mib),
            max_private_bytes: mib(max_private_bytes_mib),
            max_growth_bytes: mib(max_growth_mib),
        }
    }
}

fn parse_bounded(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    let Some(raw) = lookup(name) else {
        return Ok(default);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an integer, got {raw:?}"))?;
    if !(min..=max).contains(&value) {
        return Err(format!(
            "{name} must be between {min} and {max}, got {value}"
        ));
    }
    Ok(value)
}

fn parse_optional_bounded(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, String> {
    let Some(raw) = lookup(name) else {
        return Ok(None);
    };
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an integer, got {raw:?}"))?;
    if !(min..=max).contains(&value) {
        return Err(format!(
            "{name} must be between {min} and {max}, got {value}"
        ));
    }
    Ok(Some(value))
}

fn mib(value: u64) -> u64 {
    value.saturating_mul(MIB)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemorySample {
    pub working_set: u64,
    pub private_working_set: u64,
    pub private_bytes: u64,
}

impl MemorySample {
    #[cfg(target_os = "windows")]
    pub fn capture() -> Result<Self, String> {
        let mut sample = capture_windows_process(None)?;
        if let Some(worker_pid) = crate::mermaid::worker_pid() {
            if let Ok(worker) = capture_windows_process(Some(worker_pid)) {
                sample.working_set = sample.working_set.saturating_add(worker.working_set);
                sample.private_working_set = sample
                    .private_working_set
                    .saturating_add(worker.private_working_set);
                sample.private_bytes = sample.private_bytes.saturating_add(worker.private_bytes);
            }
        }
        Ok(sample)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn capture() -> Result<Self, String> {
        Err("memory benchmarks are currently supported on Windows only".to_owned())
    }

    #[cfg(test)]
    fn from_mib(working_set: u64, private_working_set: u64, private_bytes: u64) -> Self {
        Self {
            working_set: mib(working_set),
            private_working_set: mib(private_working_set),
            private_bytes: mib(private_bytes),
        }
    }
}

#[cfg(target_os = "windows")]
fn capture_windows_process(pid: Option<u32>) -> Result<MemorySample, String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };

    let (process, close_after) = match pid {
        Some(pid) => {
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
            if process.is_null() {
                return Err(format!(
                    "OpenProcess({pid}) failed: {}",
                    std::io::Error::last_os_error()
                ));
            }
            (process, true)
        }
        None => (unsafe { GetCurrentProcess() }, false),
    };

    let result = (|| {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
        };

        let mut counters: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { zeroed() };
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let ok = unsafe {
            GetProcessMemoryInfo(
                process,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        if ok == 0 {
            return Err(format!(
                "GetProcessMemoryInfo failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(MemorySample {
            working_set: counters.WorkingSetSize as u64,
            private_working_set: counters.PrivateWorkingSetSize as u64,
            private_bytes: counters.PrivateUsage as u64,
        })
    })();
    if close_after {
        unsafe {
            CloseHandle(process);
        }
    }
    result
}

pub struct BenchmarkMetrics {
    baseline: MemorySample,
    final_sample: MemorySample,
    peak: MemorySample,
}

impl BenchmarkMetrics {
    pub fn new(baseline: MemorySample) -> Self {
        Self {
            baseline,
            final_sample: baseline,
            peak: baseline,
        }
    }

    pub fn record(&mut self, sample: MemorySample) {
        self.final_sample = sample;
        self.peak.working_set = self.peak.working_set.max(sample.working_set);
        self.peak.private_working_set = self
            .peak
            .private_working_set
            .max(sample.private_working_set);
        self.peak.private_bytes = self.peak.private_bytes.max(sample.private_bytes);
    }

    pub fn finish(self, config: &BenchmarkConfig) -> BenchmarkReport {
        let private_bytes_growth = self
            .peak
            .private_bytes
            .saturating_sub(self.baseline.private_bytes);
        let passed = self.peak.private_working_set <= config.max_private_working_set_bytes
            && self.peak.private_bytes <= config.max_private_bytes
            && private_bytes_growth <= config.max_growth_bytes;

        BenchmarkReport {
            passed,
            baseline: self.baseline,
            final_sample: self.final_sample,
            peak_working_set: self.peak.working_set,
            peak_private_working_set: self.peak.private_working_set,
            peak_private_bytes: self.peak.private_bytes,
            final_private_bytes: self.final_sample.private_bytes,
            private_bytes_growth,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkReport {
    pub passed: bool,
    pub baseline: MemorySample,
    pub final_sample: MemorySample,
    pub peak_working_set: u64,
    pub peak_private_working_set: u64,
    pub peak_private_bytes: u64,
    pub final_private_bytes: u64,
    pub private_bytes_growth: u64,
}

impl BenchmarkReport {
    pub fn finished_line(self, scenario: BenchmarkScenario) -> String {
        format!(
            "NATIVE_MARKDOWN_BENCHMARK event=finished scenario={} result={} baseline_private_bytes_mib={:.2} final_private_bytes_mib={:.2} growth_private_bytes_mib={:.2} peak_private_bytes_mib={:.2} peak_private_ws_mib={:.2} peak_ws_mib={:.2}",
            scenario.label(),
            if self.passed { "pass" } else { "fail" },
            as_mib(self.baseline.private_bytes),
            as_mib(self.final_private_bytes),
            as_mib(self.private_bytes_growth),
            as_mib(self.peak_private_bytes),
            as_mib(self.peak_private_working_set),
            as_mib(self.peak_working_set),
        )
    }
}

pub type BenchmarkOutcome = Arc<Mutex<Option<Result<BenchmarkReport, String>>>>;

pub fn new_outcome() -> BenchmarkOutcome {
    Arc::new(Mutex::new(None))
}

pub fn start(
    config: BenchmarkConfig,
    app: WeakEntity<NativeMarkdownApp>,
    outcome: BenchmarkOutcome,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            smol::Timer::after(config.warmup).await;
            let mermaid_status = cx
                .update(|_, cx| {
                    app.update(cx, |app, _| app.mermaid_benchmark_status())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            println!(
                "NATIVE_MARKDOWN_BENCHMARK event=mermaid blocks={} ready={} pending={} errors={} worker_pid={}",
                mermaid_status.0,
                mermaid_status.1,
                mermaid_status.2,
                mermaid_status.3,
                crate::mermaid::worker_pid().unwrap_or_default(),
            );
            let baseline = match MemorySample::capture() {
                Ok(sample) => sample,
                Err(error) => {
                    finish_with_error(&outcome, &error);
                    let _ = cx.update(|_, cx| cx.quit());
                    return;
                }
            };

            println!(
                "NATIVE_MARKDOWN_BENCHMARK event=start scenario={} duration_ms={} step_ms={} baseline_private_bytes_mib={:.2}",
                config.scenario.label(),
                config.duration.as_millis(),
                config.step_interval.as_millis(),
                as_mib(baseline.private_bytes),
            );

            let started = Instant::now();
            let mut last_sample_report = Instant::now();
            let mut metrics = BenchmarkMetrics::new(baseline);
            let mut step = 0_u64;

            loop {
                if started.elapsed() >= config.duration
                    || config.max_steps.is_some_and(|max_steps| step >= max_steps)
                {
                    break;
                }
                if let Err(error) = cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        app.run_benchmark_step(
                            config.scenario,
                            step,
                            config.switch_step,
                            config.secondary_document.as_deref(),
                            window,
                            cx,
                        )
                    })
                }) {
                    finish_with_error(&outcome, &format!("benchmark UI update failed: {error}"));
                    let _ = cx.update(|_, cx| cx.quit());
                    return;
                }

                step = step.saturating_add(1);
                let remaining = config.duration.saturating_sub(started.elapsed());
                let pause = config.step_interval.min(remaining);
                if !pause.is_zero() {
                    smol::Timer::after(pause).await;
                }

                let sample = match MemorySample::capture() {
                    Ok(sample) => sample,
                    Err(error) => {
                        finish_with_error(&outcome, &error);
                        let _ = cx.update(|_, cx| cx.quit());
                        return;
                    }
                };
                metrics.record(sample);

                if last_sample_report.elapsed() >= Duration::from_secs(1) {
                    last_sample_report = Instant::now();
                    println!(
                        "NATIVE_MARKDOWN_BENCHMARK event=sample scenario={} elapsed_ms={} private_bytes_mib={:.2} private_ws_mib={:.2} ws_mib={:.2}",
                        config.scenario.label(),
                        started.elapsed().as_millis(),
                        as_mib(sample.private_bytes),
                        as_mib(sample.private_working_set),
                        as_mib(sample.working_set),
                    );
                }

            }

            let report = metrics.finish(&config);
            println!("{}", report.finished_line(config.scenario));
            *outcome.lock().expect("benchmark outcome lock poisoned") = Some(Ok(report));
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
}

fn finish_with_error(outcome: &BenchmarkOutcome, error: &str) {
    eprintln!("NATIVE_MARKDOWN_BENCHMARK event=error message={error:?}");
    *outcome.lock().expect("benchmark outcome lock poisoned") = Some(Err(error.to_owned()));
}

fn as_mib(bytes: u64) -> f64 {
    bytes as f64 / MIB as f64
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{BenchmarkConfig, BenchmarkMetrics, BenchmarkScenario, MemorySample};

    const MIB: u64 = super::MIB;

    #[test]
    fn benchmark_is_disabled_when_no_scenario_is_configured() {
        let config = BenchmarkConfig::from_lookup(|_| None).unwrap();
        assert!(config.is_none());
    }

    #[test]
    fn benchmark_configuration_parses_the_public_environment_seam() {
        let config = BenchmarkConfig::from_lookup(|name| match name {
            "NATIVE_MARKDOWN_BENCHMARK" => Some("view-modes".into()),
            "NATIVE_MARKDOWN_BENCHMARK_SECONDS" => Some("7".into()),
            "NATIVE_MARKDOWN_BENCHMARK_STEP_MS" => Some("40".into()),
            "NATIVE_MARKDOWN_BENCHMARK_STEPS" => Some("12".into()),
            "NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT" => Some("C:\\notes\\plain.md".into()),
            "NATIVE_MARKDOWN_BENCHMARK_SWITCH_STEP" => Some("25".into()),
            "NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_WS_MIB" => Some("120".into()),
            "NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_BYTES_MIB" => Some("150".into()),
            "NATIVE_MARKDOWN_BENCHMARK_MAX_GROWTH_MIB" => Some("30".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();

        assert_eq!(config.scenario, BenchmarkScenario::ViewModes);
        assert_eq!(config.duration.as_secs(), 7);
        assert_eq!(config.step_interval.as_millis(), 40);
        assert_eq!(config.max_steps, Some(12));
        assert_eq!(
            config.secondary_document,
            Some(PathBuf::from("C:\\notes\\plain.md"))
        );
        assert_eq!(config.switch_step, 25);
        assert_eq!(config.max_private_working_set_bytes, 120 * MIB);
        assert_eq!(config.max_private_bytes, 150 * MIB);
        assert_eq!(config.max_growth_bytes, 30 * MIB);
    }

    #[test]
    fn memory_budget_fails_on_the_observed_heap_growth_shape() {
        let config = BenchmarkConfig::for_test(BenchmarkScenario::Idle, 160, 160, 80);
        let mut metrics = BenchmarkMetrics::new(MemorySample::from_mib(70, 68, 82));
        metrics.record(MemorySample::from_mib(354, 266, 290));

        let report = metrics.finish(&config);

        assert!(!report.passed);
        assert_eq!(report.final_private_bytes, 290 * MIB);
        assert_eq!(report.private_bytes_growth, 208 * MIB);
    }

    #[test]
    fn memory_budget_passes_for_a_stable_native_reader() {
        let config = BenchmarkConfig::for_test(BenchmarkScenario::Idle, 100, 120, 40);
        let mut metrics = BenchmarkMetrics::new(MemorySample::from_mib(90, 60, 75));
        metrics.record(MemorySample::from_mib(115, 72, 88));

        let report = metrics.finish(&config);

        assert!(report.passed);
        assert_eq!(report.peak_private_working_set, 72 * MIB);
        assert_eq!(report.private_bytes_growth, 13 * MIB);
    }

    #[test]
    fn finished_report_is_machine_readable() {
        let config = BenchmarkConfig::for_test(BenchmarkScenario::Idle, 100, 120, 40);
        let mut metrics = BenchmarkMetrics::new(MemorySample::from_mib(90, 60, 75));
        metrics.record(MemorySample::from_mib(115, 72, 88));

        let line = metrics.finish(&config).finished_line(config.scenario);

        assert_eq!(
            line,
            "NATIVE_MARKDOWN_BENCHMARK event=finished scenario=idle result=pass baseline_private_bytes_mib=75.00 final_private_bytes_mib=88.00 growth_private_bytes_mib=13.00 peak_private_bytes_mib=88.00 peak_private_ws_mib=72.00 peak_ws_mib=115.00"
        );
    }

    #[test]
    fn benchmark_scenarios_distinguish_zoom_rendering_surfaces() {
        assert_eq!(
            BenchmarkScenario::parse("zoom-source").unwrap(),
            BenchmarkScenario::ZoomSource
        );
        assert_eq!(
            BenchmarkScenario::parse("zoom-split").unwrap(),
            BenchmarkScenario::ZoomSplit
        );
    }

    #[test]
    fn image_release_requires_a_secondary_document() {
        let error = BenchmarkConfig::from_lookup(|name| match name {
            "NATIVE_MARKDOWN_BENCHMARK" => Some("image-release".into()),
            _ => None,
        })
        .unwrap_err();

        assert!(error.contains("SECONDARY_DOCUMENT"));
    }
}
