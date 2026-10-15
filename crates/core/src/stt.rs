//! Speech-to-text interface (F-54). Engines live in `callscribe-stt-*` crates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    /// Track «Я» (microphone).
    Me,
    /// Track «Собеседник» (system sound).
    Remote,
    /// Single-track recording: no speaker separation.
    Mixed,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Я",
            Speaker::Remote => "Собеседник",
            Speaker::Mixed => "Говорящий",
        }
    }
}

/// Label with an optional diarization number (F-60): «Собеседник 2». `index` is 1-based.
pub fn speaker_label(speaker: Speaker, index: Option<u8>) -> String {
    match index {
        Some(i) => format!("{} {i}", speaker.label()),
        None => speaker.label().to_string(),
    }
}

/// A segment as returned by an engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Probability that the segment has no speech (Whisper), if the engine reports it.
    pub no_speech_prob: Option<f32>,
    /// Mean log-probability of the segment's tokens, if the engine reports it.
    pub avg_logprob: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// `None` — auto-detect.
    pub language: Option<String>,
    pub beam_size: u32,
    pub initial_prompt: Option<String>,
    pub vad: bool,
}

/// Engine info written to `meta.json` (`pipeline.stt`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineInfo {
    pub engine: String,
    pub model: String,
    pub device: String,
    pub backend: Option<String>,
    pub language: Option<String>,
}

pub trait Transcriber {
    fn info(&self) -> EngineInfo;
    /// `pcm` — 16 kHz mono f32. `progress` gets 0..=100.
    fn transcribe(
        &mut self,
        pcm: &[f32],
        opts: &TranscribeOptions,
        progress: &mut dyn FnMut(f32),
    ) -> anyhow::Result<Vec<RawSegment>>;
}
