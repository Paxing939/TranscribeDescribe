//! Output files (F-58, §7.2, §7.5).

use serde::Serialize;

use crate::pipeline::postprocess::{Segment, Utterance};
use crate::pipeline::summarize::Summary;
use crate::stt::{EngineInfo, Speaker, speaker_label};

pub fn ts_hms(sec: f64) -> String {
    let s = sec.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn ts_srt(sec: f64) -> String {
    let ms = (sec.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

pub fn duration_human(sec: f64) -> String {
    let m = (sec / 60.0).round() as u64;
    if m >= 60 {
        format!("{} ч {} мин", m / 60, m % 60)
    } else {
        format!("{m} мин")
    }
}

pub struct Header<'a> {
    pub title: &'a str,
    pub app: &'a str,
    pub started_local: &'a str,
    pub duration_sec: f64,
    pub model: &'a str,
}

/// `transcript.md` (§7.2).
pub fn transcript_md(h: &Header, utts: &[Utterance], warnings: &[String]) -> String {
    let mut s = format!(
        "# Транскрипт: {}\n{} · {} · {} · {}\n\n",
        h.title,
        h.app,
        h.started_local,
        duration_human(h.duration_sec),
        h.model
    );
    for w in warnings {
        s.push_str(&format!("> ⚠ {w}\n\n"));
    }
    for u in utts {
        s.push_str(&format!(
            "**[{}] {}:** {}\n\n",
            ts_hms(u.start),
            speaker_label(u.speaker, u.speaker_index),
            u.text
        ));
    }
    s
}

#[derive(Serialize)]
struct TranscriptJson<'a> {
    schema_version: u32,
    stt: &'a EngineInfo,
    segments: &'a [Segment],
    utterances: &'a [Utterance],
}

/// `transcript.json`: every segment (kept and dropped) plus merged lines.
pub fn transcript_json(stt: &EngineInfo, segs: &[Segment], utts: &[Utterance]) -> String {
    serde_json::to_string_pretty(&TranscriptJson {
        schema_version: 1,
        stt,
        segments: segs,
        utterances: utts,
    })
    .expect("serializable")
}

/// Wrap into lines of at most `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for w in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// `.srt` from segments, not utterances: ≤ 2 lines of ~42 chars with a speaker prefix (F-58).
pub fn srt(segs: &[Segment], with_speaker: bool) -> String {
    let mut kept: Vec<&Segment> = segs.iter().filter(|s| s.kept()).collect();
    kept.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = String::new();
    let mut n = 0;
    for s in kept {
        let text = if with_speaker && (s.speaker != Speaker::Mixed || s.speaker_index.is_some()) {
            format!("{}: {}", speaker_label(s.speaker, s.speaker_index), s.text)
        } else {
            s.text.clone()
        };
        let lines = wrap(&text, 42);
        // Split long segments into cues of two lines, time proportional to characters.
        let chunks: Vec<&[String]> = lines.chunks(2).collect();
        let total: usize = lines
            .iter()
            .map(|l| l.chars().count())
            .sum::<usize>()
            .max(1);
        let mut t = s.start;
        for chunk in chunks {
            let len: usize = chunk.iter().map(|l| l.chars().count()).sum();
            let dt = (s.end - s.start).max(0.5) * len as f64 / total as f64;
            n += 1;
            out.push_str(&format!(
                "{n}\n{} --> {}\n{}\n\n",
                ts_srt(t),
                ts_srt(t + dt),
                chunk.join("\n")
            ));
            t += dt;
        }
    }
    out
}

pub struct SummaryHeader<'a> {
    pub started_local: &'a str,
    pub duration_sec: f64,
    pub app: &'a str,
    pub user_name: &'a str,
    pub video_file: Option<&'a str>,
    pub warnings: &'a [String],
}

/// `summary.md` (§7.5, F-77): empty sections are not printed.
pub fn summary_md(sum: &Summary, h: &SummaryHeader, bookmarks: &[(f64, String)]) -> String {
    let mut s = format!("# {}\n\n", sum.title.trim());
    let tags: Vec<String> = sum
        .tags
        .iter()
        .map(|t| format!("#{}", crate::pipeline::naming::slug(t, false)))
        .collect();
    s.push_str(&format!(
        "**Дата:** {} · **Длительность:** {} · **Приложение:** {}",
        h.started_local,
        duration_human(h.duration_sec),
        h.app
    ));
    if !tags.is_empty() {
        s.push_str(&format!(" · **Теги:** {}", tags.join(" ")));
    }
    s.push_str("\n\n");
    for w in h.warnings {
        s.push_str(&format!("> ⚠ {w}\n\n"));
    }
    if !sum.summary.trim().is_empty() {
        s.push_str(&format!("## Кратко\n{}\n\n", sum.summary.trim()));
    }
    if !sum.participants.is_empty() {
        s.push_str("## Участники\n");
        for p in &sum.participants {
            let me = p.name.trim().eq_ignore_ascii_case(h.user_name.trim());
            match (&p.role, me) {
                (_, true) => s.push_str(&format!("- {} (я)\n", p.name)),
                (Some(r), false) if !r.trim().is_empty() => {
                    s.push_str(&format!("- {} — {}\n", p.name, r))
                }
                _ => s.push_str(&format!("- {}\n", p.name)),
            }
        }
        s.push('\n');
    }
    let list = |s: &mut String, title: &str, items: &[String]| {
        let items: Vec<&String> = items.iter().filter(|i| !i.trim().is_empty()).collect();
        if !items.is_empty() {
            s.push_str(&format!("## {title}\n"));
            for i in items {
                s.push_str(&format!("- {}\n", i.trim()));
            }
            s.push('\n');
        }
    };
    list(&mut s, "Ключевые моменты", &sum.key_points);
    list(&mut s, "Решения", &sum.decisions);
    if !sum.action_items.is_empty() {
        s.push_str("## Задачи\n");
        for a in &sum.action_items {
            let mut line = String::from("- [ ] ");
            if let Some(o) = a.owner.as_deref().filter(|o| !o.trim().is_empty()) {
                let o = if o.trim().eq_ignore_ascii_case(h.user_name.trim()) {
                    "Я"
                } else {
                    o
                };
                line.push_str(&format!("**{o}** — "));
            }
            line.push_str(a.task.trim());
            if let Some(d) = a.due.as_deref().filter(|d| !d.trim().is_empty()) {
                line.push_str(&format!(" — *{d}*"));
            }
            s.push_str(&line);
            s.push('\n');
        }
        s.push('\n');
    }
    list(&mut s, "Открытые вопросы", &sum.open_questions);
    if !bookmarks.is_empty() {
        s.push_str("## Отмеченные моменты\n");
        for (at, frag) in bookmarks {
            s.push_str(&format!("- [{}] «{}»\n", ts_hms(*at), frag));
        }
        s.push('\n');
    }
    s.push_str("---\n[Транскрипт](transcript.md)");
    if let Some(v) = h.video_file {
        s.push_str(&format!(" · [Запись]({})", v.replace(' ', "%20")));
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::summarize::{ActionItem, Participant};

    #[test]
    fn srt_format() {
        let segs = vec![Segment {
            start: 1.0,
            end: 3.5,
            speaker: Speaker::Me,
            speaker_index: None,
            text: "Привет, меня слышно?".into(),
            no_speech_prob: None,
            avg_logprob: None,
            dropped: None,
        }];
        assert_eq!(
            srt(&segs, true),
            "1\n00:00:01,000 --> 00:00:03,500\nЯ: Привет, меня слышно?\n\n"
        );
    }

    #[test]
    fn long_segment_splits_into_two_line_cues() {
        let text = "слово ".repeat(40);
        let segs = vec![Segment {
            start: 0.0,
            end: 20.0,
            speaker: Speaker::Remote,
            speaker_index: None,
            text,
            no_speech_prob: None,
            avg_logprob: None,
            dropped: None,
        }];
        let out = srt(&segs, true);
        for cue in out.split("\n\n").filter(|c| !c.is_empty()) {
            let lines: Vec<&str> = cue.lines().skip(2).collect();
            assert!(lines.len() <= 2);
            assert!(lines.iter().all(|l| l.chars().count() <= 42));
        }
    }

    #[test]
    fn summary_skips_empty_sections() {
        let sum = Summary {
            title: "Обсуждение CI".into(),
            summary: "Договорились.".into(),
            key_points: vec![],
            decisions: vec!["Делаем кэш".into()],
            action_items: vec![ActionItem {
                owner: Some("Илья".into()),
                task: "настроить кэш".into(),
                due: Some("до пятницы".into()),
            }],
            open_questions: vec![],
            participants: vec![
                Participant {
                    name: "Илья".into(),
                    role: None,
                },
                Participant {
                    name: "Сергей".into(),
                    role: Some("DevOps".into()),
                },
            ],
            tags: vec!["CI".into()],
        };
        let h = SummaryHeader {
            started_local: "09.10.2026 14:30",
            duration_sec: 2700.0,
            app: "Zoom",
            user_name: "Илья",
            video_file: Some("a b.mp4"),
            warnings: &[],
        };
        let md = summary_md(&sum, &h, &[]);
        assert!(md.contains("## Решения"));
        assert!(!md.contains("## Ключевые моменты"));
        assert!(md.contains("- Илья (я)"));
        assert!(md.contains("- Сергей — DevOps"));
        assert!(md.contains("- [ ] **Я** — настроить кэш — *до пятницы*"));
        assert!(md.contains("#ci"));
        assert!(md.contains("(a%20b.mp4)"));
    }
}
