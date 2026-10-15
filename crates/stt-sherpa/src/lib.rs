//! `Transcriber` on sherpa-onnx: GigaAM v3 and Parakeet TDT v3 (F-61), speaker diarization (F-60).
//!
//! sherpa-onnx is linked as a prebuilt shared C library (`SHERPA_ONNX_LIB_DIR`, D-006); its DLLs
//! (`sherpa-onnx-c-api.dll`, `onnxruntime.dll`) must sit next to the exe.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use callscribe_core::models;
use callscribe_core::stt::{EngineInfo, RawSegment, TranscribeOptions, Transcriber};
use sherpa_onnx::{
    FastClusteringConfig, OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    OfflineTransducerModelConfig, SileroVadModelConfig, SpeakerEmbeddingExtractorConfig,
    VadModelConfig, VoiceActivityDetector,
};

const SAMPLE_RATE: usize = 16_000;
/// Silero VAD works in windows of 512 samples at 16 kHz.
const VAD_WINDOW: usize = 512;
/// GigaAM takes up to ~25 s per call: longer speech is cut at this length by the VAD.
const MAX_CHUNK_SEC: f32 = 20.0;
/// Chunks shorter than this are noise clicks, not speech.
const MIN_CHUNK_SAMPLES: usize = SAMPLE_RATE / 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SherpaModel {
    GigaAm,
    Parakeet,
}

impl SherpaModel {
    pub fn from_engine(name: &str) -> Option<Self> {
        match name {
            "gigaam" => Some(Self::GigaAm),
            "parakeet" => Some(Self::Parakeet),
            _ => None,
        }
    }

    /// Folder name in `models_dir`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::GigaAm => models::GIGAAM_DIR,
            Self::Parakeet => models::PARAKEET_DIR,
        }
    }

    pub fn engine(self) -> &'static str {
        match self {
            Self::GigaAm => "gigaam",
            Self::Parakeet => "parakeet",
        }
    }
}

pub fn version() -> &'static str {
    sherpa_onnx::version()
}

fn path_str(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("путь не в UTF-8: {}", p.display()))
}

fn threads() -> i32 {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (n / 2).clamp(2, 8) as i32
}

/// `<base>.int8.onnx` if present, else `<base>.onnx`.
fn onnx_file(dir: &Path, base: &str) -> Result<PathBuf> {
    for name in [format!("{base}.int8.onnx"), format!("{base}.onnx")] {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("в {} нет файла {base}.onnx", dir.display())
}

pub struct SherpaTranscriber {
    recognizer: OfflineRecognizer,
    model: SherpaModel,
    vad_model: PathBuf,
}

impl SherpaTranscriber {
    /// `models_dir` holds the unpacked model folder and `silero_vad.onnx` (see `callscribe_core::models`).
    pub fn load(model: SherpaModel, models_dir: &Path) -> Result<Self> {
        let dir = models_dir.join(model.dir_name());
        let vad_model = models_dir.join(models::SHERPA_VAD);
        if !vad_model.is_file() {
            bail!("нет модели VAD: {}", vad_model.display());
        }
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(path_str(&onnx_file(&dir, "encoder")?)?),
                    decoder: Some(path_str(&onnx_file(&dir, "decoder")?)?),
                    joiner: Some(path_str(&onnx_file(&dir, "joiner")?)?),
                },
                tokens: Some(path_str(&dir.join("tokens.txt"))?),
                num_threads: threads(),
                provider: Some("cpu".into()),
                model_type: Some("nemo_transducer".into()),
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            ..Default::default()
        };
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            anyhow!(
                "sherpa-onnx не смог загрузить модель {} (подробности в stderr)",
                dir.display()
            )
        })?;
        Ok(SherpaTranscriber {
            recognizer,
            model,
            vad_model,
        })
    }

    fn vad(&self, total_sec: f32) -> Result<VoiceActivityDetector> {
        let cfg = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(path_str(&self.vad_model)?),
                threshold: 0.5,
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                window_size: VAD_WINDOW as i32,
                max_speech_duration: MAX_CHUNK_SEC,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        // The buffer only has to hold what is not yet popped; we drain after every window.
        let buf = total_sec.clamp(60.0, 600.0);
        VoiceActivityDetector::create(&cfg, buf)
            .ok_or_else(|| anyhow!("sherpa-onnx: не удалось создать Silero VAD"))
    }

    fn decode(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}

impl Transcriber for SherpaTranscriber {
    fn info(&self) -> EngineInfo {
        EngineInfo {
            engine: self.model.engine().into(),
            model: self.model.dir_name().into(),
            device: "cpu".into(),
            backend: Some("onnxruntime-cpu".into()),
            language: None,
        }
    }

    /// The speech is cut by Silero VAD into chunks of up to 20 s (GigaAM limit) and decoded one by one.
    /// `opts.language` / `beam_size` / `initial_prompt` are not used: GigaAM is Russian-only,
    /// Parakeet detects the language itself; decoding is greedy.
    fn transcribe(
        &mut self,
        pcm: &[f32],
        _opts: &TranscribeOptions,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<RawSegment>> {
        let total = pcm.len().max(1);
        let vad = self.vad(total as f32 / SAMPLE_RATE as f32)?;
        let mut out = Vec::new();
        let drain = |vad: &VoiceActivityDetector, out: &mut Vec<RawSegment>| {
            while let Some(seg) = vad.front() {
                let samples = seg.samples();
                if samples.len() >= MIN_CHUNK_SAMPLES {
                    let text = self.decode(samples);
                    if !text.is_empty() {
                        let start = seg.start().max(0) as f64 / SAMPLE_RATE as f64;
                        out.push(RawSegment {
                            start,
                            end: start + samples.len() as f64 / SAMPLE_RATE as f64,
                            text,
                            no_speech_prob: None,
                            avg_logprob: None,
                        });
                    }
                }
                drop(seg);
                vad.pop();
            }
        };
        let mut pos = 0;
        let mut last_pct = -1.0f32;
        while pos < pcm.len() {
            let end = (pos + VAD_WINDOW).min(pcm.len());
            if end - pos == VAD_WINDOW {
                vad.accept_waveform(&pcm[pos..end]);
            } else {
                // Pad the tail to a whole window.
                let mut tail = pcm[pos..end].to_vec();
                tail.resize(VAD_WINDOW, 0.0);
                vad.accept_waveform(&tail);
            }
            pos = end;
            drain(&vad, &mut out);
            let pct = (pos as f32 * 100.0 / total as f32).floor();
            if pct != last_pct {
                last_pct = pct;
                progress(pct.min(99.0));
            }
        }
        vad.flush();
        drain(&vad, &mut out);
        progress(100.0);
        Ok(out)
    }
}

/// Paths of the diarization models inside `models_dir` (they may not exist yet).
pub fn diarization_models(models_dir: &Path) -> (PathBuf, PathBuf) {
    let seg_dir = models_dir.join(models::PYANNOTE_DIR);
    let seg = if seg_dir.join("model.int8.onnx").is_file() {
        seg_dir.join("model.int8.onnx")
    } else {
        seg_dir.join("model.onnx")
    };
    (seg, models_dir.join(models::SPEAKER_EMBEDDING))
}

/// Offline speaker diarization (F-60): pyannote segmentation 3.0 + 3D-Speaker embeddings.
/// `pcm` — 16 kHz mono f32. Returns `(start_sec, end_sec, speaker)` sorted by start, with speaker
/// numbers 0.. as numbered by sherpa-onnx. The models must already be in `models_dir`.
pub fn diarize(pcm: &[f32], models_dir: &Path) -> Result<Vec<(f64, f64, usize)>> {
    let (seg, emb) = diarization_models(models_dir);
    for p in [&seg, &emb] {
        if !p.is_file() {
            bail!("нет модели диаризации: {}", p.display());
        }
    }
    let cfg = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(path_str(&seg)?),
                ..Default::default()
            },
            num_threads: threads(),
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(path_str(&emb)?),
            num_threads: threads(),
            ..Default::default()
        },
        clustering: FastClusteringConfig {
            num_clusters: -1,
            threshold: 0.5,
            ..Default::default()
        },
        ..Default::default()
    };
    let sd = OfflineSpeakerDiarization::create(&cfg)
        .ok_or_else(|| anyhow!("sherpa-onnx не смог создать диаризатор"))?;
    if sd.sample_rate() != SAMPLE_RATE as i32 {
        bail!("диаризатор ждёт {} Гц", sd.sample_rate());
    }
    let result = sd
        .process(pcm)
        .context("sherpa-onnx: диаризация вернула ошибку")?;
    Ok(result
        .sort_by_start_time()
        .into_iter()
        .map(|s| (s.start as f64, s.end as f64, s.speaker.max(0) as usize))
        .collect())
}
