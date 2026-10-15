//! `Transcriber` on whisper.cpp via whisper-rs (F-54).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use callscribe_core::stt::{EngineInfo, RawSegment, TranscribeOptions, Transcriber};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadParams,
};

pub fn backend_name() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else {
        "cpu"
    }
}

/// Route whisper.cpp / ggml logs to `log`; our tracing picks them up or drops them.
pub fn quiet_native_logs() {
    whisper_rs::install_logging_hooks();
}

pub struct WhisperTranscriber {
    ctx: WhisperContext,
    model_name: String,
    use_gpu: bool,
    vad_model: Option<PathBuf>,
    threads: i32,
}

impl WhisperTranscriber {
    /// Load a GGML model. With `use_gpu` the backend compiled in (Vulkan / CUDA) is used.
    pub fn load(
        model_path: &Path,
        model_name: &str,
        use_gpu: bool,
        vad_model: Option<PathBuf>,
    ) -> Result<Self> {
        let mut p = WhisperContextParameters::default();
        p.use_gpu(use_gpu);
        p.flash_attn(use_gpu);
        let ctx = WhisperContext::new_with_params(model_path, p)
            .with_context(|| format!("не удалось загрузить модель {}", model_path.display()))?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        // Physical cores ≈ logical / 2 on SMT machines; whisper.cpp scales poorly past that.
        let threads = (threads / 2).clamp(2, 8) as i32;
        Ok(WhisperTranscriber {
            ctx,
            model_name: model_name.to_string(),
            use_gpu,
            vad_model,
            threads,
        })
    }
}

impl Transcriber for WhisperTranscriber {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            engine: "whisper".into(),
            model: self.model_name.clone(),
            device: if self.use_gpu {
                "gpu".into()
            } else {
                "cpu".into()
            },
            backend: Some(if self.use_gpu {
                backend_name().to_string()
            } else {
                "cpu".into()
            }),
            language: None,
        }
    }

    fn transcribe(
        &mut self,
        pcm: &[f32],
        opts: &TranscribeOptions,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<RawSegment>> {
        let mut state = self.ctx.create_state().context("whisper: create_state")?;
        let strategy = if opts.beam_size > 1 {
            SamplingStrategy::BeamSearch {
                beam_size: opts.beam_size as i32,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(self.threads);
        params.set_language(opts.language.as_deref());
        params.set_no_context(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // Segment t0 in Whisper is the end of the previous segment (silence included);
        // token timestamps give where speech actually starts.
        params.set_token_timestamps(true);
        // Do not let whisper.cpp drop "no speech" windows silently: we filter ourselves and keep what we drop (§11, п. 9).
        params.set_no_speech_thold(1.01);
        if let Some(p) = opts.initial_prompt.as_deref().filter(|p| !p.is_empty()) {
            params.set_initial_prompt(p);
        }
        let vad_path = self
            .vad_model
            .as_ref()
            .and_then(|p| p.to_str())
            .map(str::to_string);
        if opts.vad
            && let Some(vp) = vad_path.as_deref()
        {
            params.set_vad_model_path(Some(vp));
            params.enable_vad(true);
            let mut v = WhisperVadParams::new();
            v.set_min_silence_duration(500);
            v.set_speech_pad(200);
            params.set_vad_params(v);
        }
        let pct = Arc::new(AtomicI32::new(0));
        let pct2 = pct.clone();
        params.set_progress_callback_safe(move |p: i32| pct2.store(p, Ordering::Relaxed));

        // whisper `full` blocks for minutes: run it on a scoped thread and report progress from here (§11, п. 34).
        let result = std::thread::scope(|s| {
            let h = s.spawn(|| state.full(params, pcm));
            let mut last = -1;
            while !h.is_finished() {
                let p = pct.load(Ordering::Relaxed);
                if p != last {
                    progress(p as f32);
                    last = p;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            h.join()
        });
        result
            .map_err(|_| anyhow::anyhow!("whisper: поток распознавания упал"))?
            .context("whisper: full")?;
        progress(100.0);

        let eot = self.ctx.token_eot();
        let mut out = Vec::new();
        for seg in state.as_iter() {
            let text = seg
                .to_str_lossy()
                .map(|c| c.into_owned())
                .unwrap_or_default();
            let mut sum = 0f32;
            let mut n = 0;
            let (mut t0, mut t1) = (i64::MAX, i64::MIN);
            for i in 0..seg.n_tokens() {
                if let Some(t) = seg.get_token(i) {
                    let d = t.token_data();
                    if d.id < eot {
                        sum += d.plog;
                        n += 1;
                        if d.t0 >= 0 && d.t1 > d.t0 {
                            t0 = t0.min(d.t0);
                            t1 = t1.max(d.t1);
                        }
                    }
                }
            }
            let (s0, s1) = (seg.start_timestamp(), seg.end_timestamp());
            // Use token bounds only when they are sane and inside the segment.
            let (start, end) = if t0 != i64::MAX && t0 >= s0 && t1 <= s1 + 50 && t1 > t0 {
                (t0, t1.min(s1))
            } else {
                (s0, s1)
            };
            out.push(RawSegment {
                start: start as f64 / 100.0,
                end: end as f64 / 100.0,
                text,
                no_speech_prob: Some(seg.no_speech_probability()),
                avg_logprob: (n > 0).then(|| sum / n as f32),
            });
        }
        Ok(out)
    }
}
