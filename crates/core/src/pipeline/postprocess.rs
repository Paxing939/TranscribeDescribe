//! Hallucination filter (F-55), echo dedupe (F-56), merging into utterances (F-57).

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::Transcription;
use crate::stt::{RawSegment, Speaker};

/// A segment in `transcript.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub speaker: Speaker,
    /// Diarization number within the speaker's track, 1-based (F-60); `None` — not diarized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_index: Option<u8>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_speech_prob: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_logprob: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped: Option<String>,
}

impl Segment {
    pub fn from_raw(r: RawSegment, speaker: Speaker) -> Segment {
        Segment {
            start: r.start,
            end: r.end,
            speaker,
            speaker_index: None,
            text: r.text.trim().to_string(),
            no_speech_prob: r.no_speech_prob,
            avg_logprob: r.avg_logprob,
            dropped: None,
        }
    }
    pub fn kept(&self) -> bool {
        self.dropped.is_none()
    }
}

/// A merged line of the dialogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Utterance {
    pub start: f64,
    pub end: f64,
    pub speaker: Speaker,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker_index: Option<u8>,
    pub text: String,
}

/// Typical Whisper hallucinations on silence (subtitle credits from its training data). Always applied,
/// on top of `transcription.hallucination_blacklist`: a user config keeps its own copy of that list.
const BUILTIN_BLACKLIST: &[&str] = &[
    r"(?i)^(корректор|редактор)\s+[а-яё]\.\s*[а-яё]+[.!]*(\s+(корректор|редактор)\s+[а-яё]\.\s*[а-яё]+[.!]*)*$",
    r"(?i)субтитр\w*\s+(сделал|создавал|делал|подготовил|подогнал|перевел|перевёл)",
    r"(?i)dimatorzok",
    r"(?i)^продолжение следует[.!…]*$",
    r"(?i)^спасибо за (просмотр|внимание)[.!]*$",
    r"(?i)^подпис\w+ на (наш )?канал",
    r"(?i)^(редактор|корректор) субтитров",
];

/// Thresholds for the no-speech filter; the same defaults as Whisper uses internally.
pub const NO_SPEECH_THOLD: f32 = 0.6;
pub const LOGPROB_THOLD: f32 = -1.0;

pub struct PostProcessor {
    blacklist: Vec<Regex>,
    echo: crate::config::EchoDedupe,
    merge_gap: f64,
}

impl PostProcessor {
    pub fn new(cfg: &Transcription) -> anyhow::Result<Self> {
        Ok(PostProcessor {
            blacklist: BUILTIN_BLACKLIST
                .iter()
                .copied()
                .chain(cfg.hallucination_blacklist.iter().map(String::as_str))
                .map(Regex::new)
                .collect::<Result<_, _>>()?,
            echo: cfg.echo_dedupe.clone(),
            merge_gap: cfg.merge_gap_sec,
        })
    }

    /// Mark hallucinations and empty segments (F-55). Nothing is removed — only marked.
    pub fn filter_hallucinations(&self, segs: &mut [Segment]) {
        for s in segs.iter_mut().filter(|s| s.kept()) {
            let text = s.text.trim();
            if text.is_empty() || text.chars().all(|c| !c.is_alphanumeric()) {
                s.dropped = Some("empty".into());
            } else if is_non_speech_tag(text) {
                // Whisper's [BLANK_AUDIO], (музыка), *шум* and the like.
                s.dropped = Some("non_speech".into());
            } else if self.blacklist.iter().any(|re| re.is_match(text)) {
                s.dropped = Some("blacklist".into());
            } else if let (Some(ns), Some(lp)) = (s.no_speech_prob, s.avg_logprob)
                && ns > NO_SPEECH_THOLD
                && lp < LOGPROB_THOLD
            {
                s.dropped = Some("no_speech".into());
            }
        }
    }

    /// Drop «Я» segments that repeat an overlapping «Собеседник» segment (F-56).
    pub fn dedupe_echo(&self, segs: &mut [Segment]) {
        if !self.echo.enabled {
            return;
        }
        let remote: Vec<(f64, f64, String)> = segs
            .iter()
            .filter(|s| s.kept() && s.speaker == Speaker::Remote)
            .map(|s| (s.start, s.end, normalize(&s.text)))
            .collect();
        let off = self.echo.max_offset_sec;
        let min_sim = self.echo.similarity / 100.0;
        for s in segs
            .iter_mut()
            .filter(|s| s.kept() && s.speaker == Speaker::Me)
        {
            let me = normalize(&s.text);
            if me.is_empty() {
                continue;
            }
            let echo = remote.iter().any(|(rs, re, rt)| {
                s.start <= re + off && rs - off <= s.end && similarity(&me, rt) >= min_sim
            });
            if echo {
                s.dropped = Some("echo".into());
            }
        }
    }

    /// Sort by time and merge neighbouring kept segments of one speaker (F-57).
    pub fn merge(&self, segs: &[Segment]) -> Vec<Utterance> {
        let mut kept: Vec<&Segment> = segs.iter().filter(|s| s.kept()).collect();
        kept.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut out: Vec<Utterance> = Vec::new();
        for s in kept {
            if let Some(last) = out.last_mut()
                && last.speaker == s.speaker
                && last.speaker_index == s.speaker_index
                && s.start - last.end < self.merge_gap
            {
                last.end = last.end.max(s.end);
                last.text.push(' ');
                last.text.push_str(&s.text);
                continue;
            }
            out.push(Utterance {
                start: s.start,
                end: s.end,
                speaker: s.speaker,
                speaker_index: s.speaker_index,
                text: s.text.clone(),
            });
        }
        out
    }

    pub fn run(&self, segs: &mut [Segment]) -> Vec<Utterance> {
        segs.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.filter_hallucinations(segs);
        self.dedupe_echo(segs);
        self.merge(segs)
    }
}

/// Label the segments of one track with diarization turns `(start, end, speaker)` (F-60).
/// Each segment gets the speaker whose turns overlap it most (or the nearest turn); speakers are
/// numbered 1.. in order of first appearance. With a single speaker nothing is labelled.
/// Returns the number of distinct speakers.
pub fn assign_speakers(segs: &mut [Segment], turns: &[(f64, f64, usize)]) -> usize {
    let mut turns: Vec<(f64, f64, usize)> = turns.to_vec();
    turns.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut order: Vec<usize> = Vec::new();
    for t in &turns {
        if !order.contains(&t.2) {
            order.push(t.2);
        }
    }
    if order.len() < 2 {
        return order.len();
    }
    for s in segs.iter_mut() {
        let mut overlap = vec![0.0f64; order.len()];
        for t in &turns {
            let o = s.end.min(t.1) - s.start.max(t.0);
            if o > 0.0 {
                overlap[order.iter().position(|&x| x == t.2).unwrap()] += o;
            }
        }
        let best = overlap
            .iter()
            .enumerate()
            .filter(|(_, o)| **o > 0.0)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .or_else(|| {
                let mid = (s.start + s.end) / 2.0;
                let dist = |t: &&(f64, f64, usize)| (t.0 - mid).max(mid - t.1).max(0.0);
                turns
                    .iter()
                    .min_by(|a, b| dist(a).total_cmp(&dist(b)))
                    .and_then(|t| order.iter().position(|&x| x == t.2))
            });
        s.speaker_index = best.map(|i| (i + 1).min(255) as u8);
    }
    order.len()
}

/// Whisper puts leading silence into a segment. Move `start`/`end` to the first/last 20 ms frame
/// louder than the track's peak − 35 dB (16 kHz mono PCM).
/// Returns, per segment, `true` if there is no sound in it at all — a hallucination on silence.
pub fn trim_to_audio(pcm: &[f32], segs: &mut [RawSegment]) -> Vec<bool> {
    const SR: f64 = 16000.0;
    const FRAME: usize = 320;
    let mut silent = vec![false; segs.len()];
    if pcm.is_empty() {
        return silent;
    }
    let rms: Vec<f32> = pcm
        .chunks(FRAME)
        .map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().cloned().fold(0f32, f32::max);
    if peak <= 0.0 {
        return silent;
    }
    let thr = peak * 10f32.powf(-35.0 / 20.0);
    for (k, s) in segs.iter_mut().enumerate() {
        let a = ((s.start * SR) as usize / FRAME).min(rms.len());
        let b = (((s.end * SR) as usize).div_ceil(FRAME)).min(rms.len());
        if a >= b {
            silent[k] = true;
            continue;
        }
        let loud: Vec<usize> = (a..b).filter(|&i| rms[i] >= thr).collect();
        match (loud.first(), loud.last()) {
            (Some(&f), Some(&l)) => {
                // Keep 100 ms of margin.
                s.start = ((f.saturating_sub(5)) * FRAME) as f64 / SR;
                s.end = (((l + 6).min(rms.len())) * FRAME) as f64 / SR;
                s.end = s.end.max(s.start + 0.2);
            }
            _ => silent[k] = true,
        }
    }
    silent
}

fn is_non_speech_tag(t: &str) -> bool {
    let t = t.trim();
    [('[', ']'), ('(', ')'), ('*', '*'), ('♪', '♪')]
        .iter()
        .any(|&(a, b)| {
            t.len() > 2
                && t.starts_with(a)
                && t.ends_with(b)
                && !t[a.len_utf8()..t.len() - b.len_utf8()].contains([a, b])
        })
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalized similarity 0..1. Partial match: a short echo inside a longer remote line counts.
pub fn similarity(a: &str, b: &str) -> f64 {
    let full = rapidfuzz::fuzz::ratio(a.chars(), b.chars());
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    let partial = if short.chars().count() >= 8 && long.contains(short) {
        1.0
    } else {
        0.0
    };
    full.max(partial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn seg(start: f64, end: f64, sp: Speaker, t: &str) -> Segment {
        Segment {
            start,
            end,
            speaker: sp,
            speaker_index: None,
            text: t.into(),
            no_speech_prob: None,
            avg_logprob: None,
            dropped: None,
        }
    }
    fn pp() -> PostProcessor {
        PostProcessor::new(&Config::default().transcription).unwrap()
    }

    #[test]
    fn diarization_labels() {
        let mut v = vec![
            seg(0.0, 2.0, Speaker::Remote, "a"),
            seg(2.5, 4.0, Speaker::Remote, "b"),
            seg(5.0, 6.0, Speaker::Remote, "c"),
        ];
        // Raw ids 7 and 3; 7 speaks first, so it becomes «1». Segment c is in a gap: nearest turn wins.
        let turns = [(0.0, 2.2, 7), (2.3, 4.1, 3), (4.2, 4.5, 3)];
        assert_eq!(assign_speakers(&mut v, &turns), 2);
        let idx: Vec<_> = v.iter().map(|s| s.speaker_index).collect();
        assert_eq!(idx, [Some(1), Some(2), Some(2)]);
        // One speaker: no numbering.
        let mut one = vec![seg(0.0, 1.0, Speaker::Remote, "x")];
        assert_eq!(assign_speakers(&mut one, &[(0.0, 1.0, 0)]), 1);
        assert_eq!(one[0].speaker_index, None);
        // Adjacent segments of different speakers are not merged into one line.
        let mut v2 = vec![
            seg(0.0, 1.0, Speaker::Remote, "a"),
            seg(1.1, 2.0, Speaker::Remote, "b"),
        ];
        assign_speakers(&mut v2, &[(0.0, 1.0, 0), (1.1, 2.0, 1)]);
        assert_eq!(pp().merge(&v2).len(), 2);
    }

    #[test]
    fn blacklist_and_no_speech() {
        let mut v = vec![
            seg(0.0, 1.0, Speaker::Me, "Продолжение следует..."),
            seg(1.0, 2.0, Speaker::Me, "Субтитры сделал DimaTorzok"),
            seg(2.0, 3.0, Speaker::Me, "Привет"),
            Segment {
                no_speech_prob: Some(0.9),
                avg_logprob: Some(-1.5),
                ..seg(3.0, 4.0, Speaker::Me, "Угу")
            },
            seg(4.0, 5.0, Speaker::Me, " ... "),
            seg(5.0, 6.0, Speaker::Remote, "[BLANK_AUDIO]"),
            seg(6.0, 7.0, Speaker::Remote, "(музыка)"),
        ];
        pp().filter_hallucinations(&mut v);
        let d: Vec<_> = v.iter().map(|s| s.dropped.as_deref()).collect();
        assert_eq!(
            d,
            vec![
                Some("blacklist"),
                Some("blacklist"),
                None,
                Some("no_speech"),
                Some("empty"),
                Some("non_speech"),
                Some("non_speech")
            ]
        );
    }

    #[test]
    fn echo_is_dropped_only_when_overlapping_and_similar() {
        let mut v = vec![
            seg(
                10.0,
                13.0,
                Speaker::Remote,
                "Давай начнём с пайплайна, там всё сломалось",
            ),
            seg(
                10.5,
                13.2,
                Speaker::Me,
                "давай начнём с пайплайна там всё сломалось",
            ),
            seg(
                30.0,
                32.0,
                Speaker::Me,
                "давай начнём с пайплайна там всё сломалось",
            ),
            seg(11.0, 12.0, Speaker::Me, "Да, согласен"),
        ];
        pp().dedupe_echo(&mut v);
        assert_eq!(v[1].dropped.as_deref(), Some("echo"));
        assert_eq!(v[2].dropped, None);
        assert_eq!(v[3].dropped, None);
    }

    #[test]
    fn builtin_blacklist_catches_subtitle_credits() {
        let mut v = vec![
            seg(0.0, 1.0, Speaker::Remote, "Корректор А.Кулакова"),
            seg(1.0, 2.0, Speaker::Remote, "Корректор А.Кулакова Корректор А.Кулакова Корректор А.Кулакова"),
            seg(2.0, 3.0, Speaker::Me, "Корректор А.Егорова"),
            seg(3.0, 4.0, Speaker::Remote, "Корректор кода у нас Антон."),
        ];
        let mut cfg = Config::default().transcription;
        cfg.hallucination_blacklist.clear();
        PostProcessor::new(&cfg).unwrap().filter_hallucinations(&mut v);
        let d: Vec<_> = v.iter().map(|s| s.dropped.as_deref()).collect();
        assert_eq!(d, vec![Some("blacklist"), Some("blacklist"), Some("blacklist"), None]);
    }

    #[test]
    fn trim_leading_silence() {
        let mut pcm = vec![0f32; 16000 * 5];
        pcm.extend(std::iter::repeat_n(0.5f32, 16000 * 2));
        pcm.extend(std::iter::repeat_n(0f32, 16000 * 3));
        let mut segs = vec![
            RawSegment {
                start: 0.0,
                end: 7.0,
                text: "x".into(),
                no_speech_prob: None,
                avg_logprob: None,
            },
            RawSegment {
                start: 7.5,
                end: 9.5,
                text: "Thank you.".into(),
                no_speech_prob: None,
                avg_logprob: None,
            },
        ];
        assert_eq!(trim_to_audio(&pcm, &mut segs), vec![false, true]);
        assert!((segs[0].start - 4.9).abs() < 0.05, "{}", segs[0].start);
        assert!((segs[0].end - 7.0).abs() < 0.15, "{}", segs[0].end);
    }

    #[test]
    fn merge_same_speaker() {
        let v = vec![
            seg(0.0, 1.0, Speaker::Me, "Привет,"),
            seg(1.5, 2.0, Speaker::Me, "меня слышно?"),
            seg(2.2, 3.0, Speaker::Remote, "Да."),
            seg(10.0, 11.0, Speaker::Remote, "Начнём."),
        ];
        let u = pp().merge(&v);
        assert_eq!(u.len(), 3);
        assert_eq!(u[0].text, "Привет, меня слышно?");
    }
}
