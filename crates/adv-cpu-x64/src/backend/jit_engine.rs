//! The Cranelift engine behind the host-facing [`Engine`] interface.
use std::collections::BTreeMap;
use std::path::Path;

use super::jit::JitCpu;
use crate::engine::{Engine, EngineCounters, EngineOptions, json_string as json};
use crate::{Cliffs, ProfileSample, VerifyStats};

const WARM_PROFILE: &str = "jit-warm.bin";
const CODE_CACHE: &str = "jit-code.bin";

impl Engine for JitCpu {
    fn create(park: u32, native: bool) -> Result<Self, String> {
        Self::new(park, native)
    }

    fn configure(&mut self, options: &EngineOptions) -> Result<(), String> {
        let native = self.native();
        if !options.row_batching || !native {
            self.disable_row_batching();
        } else if let Some(workers) = options.row_workers {
            self.set_row_workers(workers)?;
        }
        self.set_auto_compile_strategy(options.compile_strategy);
        let async_compilation = native && options.async_compilation;
        let executor = self.executor_mut();
        executor.set_tiered_compilation(options.tiered_compilation);
        executor.set_region_linking(options.region_linking);
        executor.set_async_compilation(async_compilation);
        executor.set_aot_enabled(native && options.precompile);
        let artifacts = options.artifacts.as_deref();
        if let Some(dir) = artifacts.filter(|_| native && options.code_cache) {
            executor.set_persistent_code_cache(&dir.join(CODE_CACHE))?;
        }
        if async_compilation {
            let threads = match options.compile_threads {
                Some(threads) => threads,
                None => compile_thread_override(
                    std::env::var("ADV_COMPILE_THREADS").ok().as_deref(),
                    std::thread::available_parallelism()
                        .map_or(1, |cores| compile_thread_count(cores.get())),
                )?,
            };
            executor.set_compile_threads(threads)?;
            // Guest regions resident at the previous exit are compiled in the
            // background as soon as identical code is mapped.
            if let Some(profile) =
                artifacts.and_then(|dir| std::fs::read(dir.join(WARM_PROFILE)).ok())
            {
                executor.load_warm_profile(&profile);
            }
        }
        if options.frame_timing {
            executor.enable_frame_timing(true);
        }
        executor.set_profiling(options.profiling);
        self.set_artifacts(
            artifacts
                .filter(|_| async_compilation)
                .map(Path::to_path_buf),
        );
        Ok(())
    }

    fn set_waker(&mut self, wake: adv_sync::WakeEvent) -> Result<(), String> {
        JitCpu::set_waker(self, wake)
    }

    fn set_background_sender(
        &mut self,
        sender: adv_sync::background::LocalSender,
    ) -> Result<(), String> {
        JitCpu::set_background_sender(self, sender)
    }

    fn background_sender(&mut self) -> Result<adv_sync::background::LocalSender, String> {
        JitCpu::background_sender(self)
    }

    fn counters(&self) -> EngineCounters {
        let stats = &self.executor().stats;
        EngineCounters {
            native_entries: stats.native_entries,
            interpreter_entries: stats.interpreter_entries,
        }
    }

    fn report_fields(&self) -> String {
        let executor = self.executor();
        let stats = &executor.stats;
        format!(
            concat!(
                "\"native_entries\": {},\n  \"interpreter_entries\": {},\n  \"cpu_cliffs\": {},\n",
                "  \"tier_verification\": {},\n  \"code_cache\": {},\n  \"aot\": {},\n",
                "  \"compilation_schedule\": {},\n  \"compilations\": {},\n  \"compile_seconds\": {:.6},\n",
                "  \"compile_requests\": {},\n  \"pending_compilations\": {},\n",
                "  \"peak_pending_compilations\": {},\n  \"stale_compilations\": {},\n  \"compile_failures\": {}"
            ),
            stats.native_entries,
            stats.interpreter_entries,
            cliffs_report(&stats.cliffs),
            verification_report(&stats.verification),
            executor.persistent_code_cache_json(),
            stats.aot.json(),
            executor.compilation_schedule_json(),
            stats.compilations,
            stats.compile_time.as_secs_f64(),
            stats.compile_requests,
            stats.pending_compilations,
            stats.peak_pending_compilations,
            stats.stale_compilations,
            stats.compile_failures,
        )
    }

    fn write_report_extras(&self, report: &Path) -> Result<(), String> {
        // ADV_LOOP_COVERAGE: the loop table goes beside the report.
        if let Some(loops) = crate::loop_coverage() {
            std::fs::write(report.with_extension("loops.tsv"), loops).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn profile(&self) -> BTreeMap<u32, ProfileSample> {
        JitCpu::profile(self)
    }

    fn persist(&self) -> Result<(), String> {
        if let Some(dir) = self.artifacts() {
            let profile = self.executor().warm_profile();
            let _ = std::fs::create_dir_all(dir)
                .and_then(|_| std::fs::write(dir.join(WARM_PROFILE), profile));
        }
        self.executor()
            .save_persistent_code_cache()
            .map_err(|error| format!("native code cache save failed: {error}"))
    }
}

fn compile_thread_count(cores: usize) -> usize {
    // Keep the small-host policy. Larger desktop hosts can serve another
    // compile without consuming the cores reserved for execution, audio and
    // presentation.
    let cap = if cores >= 8 { 4 } else { 3 };
    cores.saturating_sub(2).clamp(1, cap)
}

fn compile_thread_override(value: Option<&str>, default: usize) -> Result<usize, String> {
    match value {
        None => Ok(default),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|threads| (1..=8).contains(threads))
            .ok_or_else(|| "ADV_COMPILE_THREADS must be an integer from 1 to 8".into()),
    }
}

/// `ADV_JIT_VERIFY` interpreter checks of newly published native code.
fn verification_report(v: &VerifyStats) -> String {
    format!(
        "{{\"verified\":{},\"inconclusive\":{},\"mismatches\":{},\"mxcsr_flags\":{},\"reports\":[{}]}}",
        v.verified,
        v.inconclusive,
        v.mismatches,
        v.mxcsr_flags,
        v.reports
            .iter()
            .map(|r| json(r))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Slow-path counters, each list cut to its most frequent entries.
fn cliffs_report(c: &Cliffs) -> String {
    const ROWS: usize = 24;
    let counts = |rows: Vec<(String, u64)>, limit: usize| {
        rows.into_iter()
            .take(limit)
            .map(|(key, n)| format!("{}:{n}", json(&key)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let interpreter = c
        .interpreter_rows()
        .into_iter()
        .map(|(reason, n)| {
            format!(
                "{}:{{\"entries\":{},\"instructions\":{}}}",
                json(&reason),
                n.entries,
                n.instructions
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let detail_rows = c.interpreter_detail_rows();
    let omitted_rows = detail_rows.len().saturating_sub(ROWS);
    let details = detail_rows
        .iter()
        .take(ROWS)
        .map(|(label, n)| {
            format!(
                "{}:{{\"entries\":{},\"instructions\":{}}}",
                json(label),
                n.entries,
                n.instructions
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let omitted = ["Uncovered", "Rejected"]
        .map(|reason| {
            let mut entries = 0;
            let mut instructions = 0;
            for (label, n) in detail_rows.iter().skip(ROWS) {
                if label.starts_with(&format!("{reason}/")) {
                    entries += n.entries;
                    instructions += n.instructions;
                }
            }
            format!(
                "{}:{{\"entries\":{entries},\"instructions\":{instructions}}}",
                json(reason)
            )
        })
        .join(",");
    format!(
        "{{\"x87_helper_calls\":{},\"x87_fast\":{},\"x87_exact\":{{{}}},\"x87_control\":{{{}}},\"x87_control_rejections\":{{{}}},\"x87_pc64\":{{{}}},\"fallbacks\":{{{}}},\"interpreter\":{{{}}},\"interpreter_details\":{{{details}}},\"interpreter_details_omitted\":{{{omitted}}},\"interpreter_detail_rows_omitted\":{omitted_rows}}}",
        c.x87_helper_calls,
        c.x87_fast,
        counts(c.x87_exact_rows(), ROWS),
        counts(c.x87_control_rows(), usize::MAX),
        counts(c.x87_control_rejections(), usize::MAX),
        counts(c.x87_pc64_rows(), usize::MAX),
        counts(c.fallback_rows(), ROWS),
        interpreter
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experimental_compile_threads_preserve_default_and_reject_invalid_values() {
        assert_eq!(compile_thread_override(None, 3).unwrap(), 3);
        for threads in 1..=8 {
            assert_eq!(
                compile_thread_override(Some(&threads.to_string()), 3).unwrap(),
                threads
            );
        }
        for value in ["", "0", "9", "-1", "auto"] {
            assert!(compile_thread_override(Some(value), 3).is_err());
        }
    }

    #[test]
    fn compile_thread_budget_preserves_small_hosts_and_reserves_runtime_cores() {
        for cores in 0usize..8 {
            assert_eq!(
                compile_thread_count(cores),
                cores.saturating_sub(2).clamp(1, 3)
            );
        }
        for cores in [8, 10, 16, 128] {
            assert_eq!(compile_thread_count(cores), 4);
            assert!(compile_thread_count(cores) + 2 <= cores);
        }
    }
}
