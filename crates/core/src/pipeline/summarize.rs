//! Summary via LLM (F-72…F-77): prompts, map-reduce, JSON parsing.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::llm::{LlmProvider, Message, extract_json};
use crate::pipeline::export::{duration_human, ts_hms};
use crate::pipeline::postprocess::Utterance;
use crate::stt::speaker_label;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionItem {
    #[serde(default)]
    pub owner: Option<String>,
    pub task: String,
    #[serde(default)]
    pub due: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// LLM answer, schema §7.4. `title` and `summary` are required; lists may be missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub key_points: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub action_items: Vec<ActionItem>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub participants: Vec<Participant>,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn schema() -> Value {
    json!({
      "type": "object",
      "required": ["title", "summary", "key_points", "decisions", "action_items", "open_questions", "participants", "tags"],
      "properties": {
        "title": {"type": "string", "description": "3–7 слов, суть звонка, без даты и названия приложения"},
        "summary": {"type": "string", "description": "2–4 предложения: о чём звонок и чем закончился"},
        "key_points": {"type": "array", "items": {"type": "string"}},
        "decisions": {"type": "array", "items": {"type": "string"}},
        "action_items": {"type": "array", "items": {"type": "object", "required": ["owner", "task", "due"],
          "properties": {"owner": {"type": ["string", "null"]}, "task": {"type": "string"}, "due": {"type": ["string", "null"]}}}},
        "open_questions": {"type": "array", "items": {"type": "string"}},
        "participants": {"type": "array", "items": {"type": "object", "required": ["name", "role"],
          "properties": {"name": {"type": "string"}, "role": {"type": ["string", "null"], "description": "должность, компания или зона ответственности, если прозвучали"}}}},
        "tags": {"type": "array", "items": {"type": "string"}}
      }
    })
}

fn example() -> &'static str {
    r#"{"title": "Обсуждение CI-пайплайна", "summary": "Обсудили падения сборки. Решили добавить кэш.", "key_points": ["Сборка падает из-за таймаутов"], "decisions": ["Добавить кэш зависимостей"], "action_items": [{"owner": "Илья", "task": "настроить кэш", "due": "до пятницы"}], "open_questions": [], "participants": [{"name": "Илья", "role": null}], "tags": ["ci"]}"#
}

#[derive(Debug, Clone)]
pub struct Prompts {
    pub summary: String,
    pub chunk: String,
    pub reduce: String,
}

impl Prompts {
    /// Built-in prompts, each overridable by a file in `prompts_dir` (F-76).
    pub fn load(dir: Option<&Path>) -> Prompts {
        let get = |name: &str, builtin: &str| -> String {
            dir.map(|d| d.join(name))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_else(|| builtin.to_string())
        };
        Prompts {
            summary: get("summary_ru.md", include_str!("../prompts/summary_ru.md")),
            chunk: get("chunk_ru.md", include_str!("../prompts/chunk_ru.md")),
            reduce: get("reduce_ru.md", include_str!("../prompts/reduce_ru.md")),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub app: String,
    pub date: String,
    pub duration_sec: f64,
    pub window_title: Option<String>,
    pub user_name: String,
    pub glossary: Vec<String>,
    pub language: String,
    pub title_hint: Option<String>,
    /// (time in file, transcript fragment around it).
    pub bookmarks: Vec<(f64, String)>,
}

/// Plain substitution, not a template engine: the prompt contains JSON braces (§8).
pub fn render(
    template: &str,
    ctx: &Context,
    transcript: &str,
    chunk: Option<(usize, usize)>,
) -> String {
    let bookmarks = if ctx.bookmarks.is_empty() {
        String::new()
    } else {
        let mut s = String::from("- Пользователь отметил важные моменты (учти их в протоколе):\n");
        for (at, frag) in &ctx.bookmarks {
            s.push_str(&format!("  - [{}] {}\n", ts_hms(*at), frag));
        }
        s
    };
    let title_hint = ctx
        .title_hint
        .as_deref()
        .map(|t| {
            format!("- Пользователь назвал звонок: «{t}». Используй это название как title.\n")
        })
        .unwrap_or_default();
    let (n, m) = chunk.unwrap_or((1, 1));
    template
        .replace("{app}", &ctx.app)
        .replace("{date}", &ctx.date)
        .replace("{duration}", &duration_human(ctx.duration_sec))
        .replace(
            "{window_title}",
            ctx.window_title.as_deref().unwrap_or("нет"),
        )
        .replace("{user_name}", &ctx.user_name)
        .replace(
            "{glossary}",
            &if ctx.glossary.is_empty() {
                "нет".to_string()
            } else {
                ctx.glossary.join(", ")
            },
        )
        .replace("{language}", &ctx.language)
        .replace("{title_hint}", &title_hint)
        .replace("{bookmarks}", &bookmarks)
        .replace(
            "{schema}",
            &serde_json::to_string_pretty(&schema()).unwrap(),
        )
        .replace("{example}", example())
        .replace("{chunk}", &n.to_string())
        .replace("{chunks}", &m.to_string())
        .replace("{transcript}", transcript)
}

pub fn transcript_lines(utts: &[Utterance]) -> Vec<String> {
    utts.iter()
        .map(|u| format!("[{}] {}: {}", ts_hms(u.start), speaker_label(u.speaker, u.speaker_index), u.text))
        .collect()
}

/// Rough token estimate by characters (F-73). Conservative for Cyrillic.
pub fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() as f64 / 2.5).ceil() as usize
}

/// Split lines into chunks of at most `max_tokens`, overlapping by `overlap` lines.
pub fn chunk_lines(lines: &[String], max_tokens: usize, overlap: usize) -> Vec<Vec<String>> {
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        let mut tokens = 0;
        while j < lines.len() {
            let t = estimate_tokens(&lines[j]) + 1;
            if j > i && tokens + t > max_tokens {
                break;
            }
            tokens += t;
            j += 1;
        }
        chunks.push(lines[i..j].to_vec());
        if j >= lines.len() {
            break;
        }
        i = if j - i > overlap { j - overlap } else { j };
    }
    chunks
}

/// Parse an LLM answer into `Summary`.
pub fn parse_summary(text: &str) -> Result<Summary, String> {
    let json = extract_json(text);
    let s: Summary = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if s.title.trim().is_empty() {
        return Err("пустой title".into());
    }
    Ok(s)
}

#[derive(Debug)]
pub enum SummaryResult {
    Ok(Summary),
    /// Still invalid after one retry: the raw answer goes to `summary.md` (F-72).
    Raw {
        text: String,
        error: String,
    },
}

pub struct Outcome {
    pub result: SummaryResult,
    pub chunks: u32,
}

/// Ask for the JSON summary; one retry with the error text on invalid JSON.
async fn ask_json(llm: &dyn LlmProvider, prompt: String) -> Result<SummaryResult> {
    let schema = schema();
    let mut messages = vec![Message::user(prompt)];
    let first = llm.complete(&messages, Some(&schema)).await?;
    let err = match parse_summary(&first) {
        Ok(s) => return Ok(SummaryResult::Ok(s)),
        Err(e) => e,
    };
    tracing::warn!("невалидный JSON от LLM: {err}; повтор");
    messages.push(Message::assistant(first));
    messages.push(Message::user(format!(
        "Ответ не разобрался как JSON по схеме: {err}. Ответь ещё раз только JSON по схеме, без пояснений."
    )));
    let second = llm.complete(&messages, Some(&schema)).await?;
    Ok(match parse_summary(&second) {
        Ok(s) => SummaryResult::Ok(s),
        Err(error) => SummaryResult::Raw {
            text: second,
            error,
        },
    })
}

/// Summarize a call; map-reduce when the transcript does not fit (F-73).
pub async fn summarize(
    llm: &dyn LlmProvider,
    prompts: &Prompts,
    ctx: &Context,
    utts: &[Utterance],
) -> Result<Outcome> {
    let lines = transcript_lines(utts);
    let full = lines.join("\n");
    let overhead = estimate_tokens(&render(&prompts.summary, ctx, "", None));
    let budget = (llm.context_tokens() as usize)
        .saturating_sub(llm.max_output_tokens() as usize + overhead + 256);
    if estimate_tokens(&full) <= budget {
        let result = ask_json(llm, render(&prompts.summary, ctx, &full, None)).await?;
        return Ok(Outcome { result, chunks: 1 });
    }

    let chunk_overhead = estimate_tokens(&render(&prompts.chunk, ctx, "", Some((99, 99))));
    let chunk_budget = (llm.context_tokens() as usize)
        .saturating_sub(llm.max_output_tokens() as usize + chunk_overhead + 256)
        .max(512);
    let chunks = chunk_lines(&lines, chunk_budget, 3);
    let m = chunks.len();
    tracing::info!("транскрипт не влезает в контекст: map-reduce на {m} фрагментов");
    let mut notes = Vec::with_capacity(m);
    for (i, c) in chunks.iter().enumerate() {
        let prompt = render(&prompts.chunk, ctx, &c.join("\n"), Some((i + 1, m)));
        let n = llm.complete(&[Message::user(prompt)], None).await?;
        let n = n.rsplit("</think>").next().unwrap_or(&n).trim().to_string();
        notes.push(format!("### Фрагмент {} из {m}\n{n}", i + 1));
    }
    let result = ask_json(llm, render(&prompts.reduce, ctx, &notes.join("\n\n"), None)).await?;
    Ok(Outcome {
        result,
        chunks: m as u32,
    })
}

/// Bookmarks: transcript fragment ± `window` seconds around each mark (F-74).
pub fn bookmark_fragments(utts: &[Utterance], marks: &[f64], window: f64) -> Vec<(f64, String)> {
    marks
        .iter()
        .map(|&at| {
            let text: Vec<String> = utts
                .iter()
                .filter(|u| u.end >= at - window && u.start <= at + window)
                .map(|u| format!("{}: {}", speaker_label(u.speaker, u.speaker_index), u.text))
                .collect();
            let mut t = text.join(" ");
            if t.chars().count() > 300 {
                t = t.chars().take(300).collect::<String>() + "…";
            }
            (at, t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::Speaker;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct Fake {
        answers: Mutex<Vec<String>>,
        ctx: u32,
        calls: Mutex<u32>,
    }
    #[async_trait]
    impl LlmProvider for Fake {
        fn describe(&self) -> (String, String) {
            ("fake".into(), "fake".into())
        }
        fn is_cloud(&self) -> bool {
            false
        }
        fn context_tokens(&self) -> u32 {
            self.ctx
        }
        fn max_output_tokens(&self) -> u32 {
            200
        }
        async fn complete(&self, _m: &[Message], _s: Option<&Value>) -> Result<String> {
            *self.calls.lock().unwrap() += 1;
            let mut a = self.answers.lock().unwrap();
            Ok(if a.len() > 1 {
                a.remove(0)
            } else {
                a[0].clone()
            })
        }
    }
    fn fake(answers: &[&str], ctx: u32) -> Fake {
        Fake {
            answers: Mutex::new(answers.iter().map(|s| s.to_string()).collect()),
            ctx,
            calls: Mutex::new(0),
        }
    }
    fn utts(n: usize) -> Vec<Utterance> {
        (0..n)
            .map(|i| Utterance {
                start: i as f64 * 10.0,
                end: i as f64 * 10.0 + 5.0,
                speaker: Speaker::Me,
                speaker_index: None,
                text: format!("реплика номер {i} про пайплайн и деплой"),
            })
            .collect()
    }
    const GOOD: &str = r#"{"title":"Тест","summary":"Кратко.","key_points":[],"decisions":[],"action_items":[],"open_questions":[],"participants":[],"tags":[]}"#;

    #[tokio::test]
    async fn single_pass_and_retry() {
        let f = fake(&["не json", GOOD], 100_000);
        let o = summarize(&f, &Prompts::load(None), &Context::default(), &utts(5))
            .await
            .unwrap();
        assert!(matches!(o.result, SummaryResult::Ok(_)));
        assert_eq!(*f.calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn raw_after_second_failure() {
        let f = fake(&["не json"], 100_000);
        let o = summarize(&f, &Prompts::load(None), &Context::default(), &utts(5))
            .await
            .unwrap();
        assert!(matches!(o.result, SummaryResult::Raw { .. }));
    }

    #[tokio::test]
    async fn map_reduce_when_too_long() {
        let f = fake(&["заметки"], 3000);
        let o = summarize(&f, &Prompts::load(None), &Context::default(), &utts(400)).await;
        // The fake always answers «заметки», so reduce ends with a raw result — but chunking happened.
        let o = o.unwrap();
        assert!(o.chunks > 1);
    }

    #[test]
    fn chunks_overlap_and_cover_everything() {
        let lines: Vec<String> = (0..100).map(|i| format!("строка {i} ").repeat(5)).collect();
        let c = chunk_lines(&lines, 200, 2);
        assert!(c.len() > 2);
        assert_eq!(c.first().unwrap()[0], lines[0]);
        assert_eq!(c.last().unwrap().last().unwrap(), lines.last().unwrap());
    }

    #[test]
    fn parse_with_think_and_fences() {
        let s = parse_summary(&format!("<think>x</think>```json\n{GOOD}\n```")).unwrap();
        assert_eq!(s.title, "Тест");
    }

    #[test]
    fn render_keeps_json_braces() {
        let r = render(
            "{schema} {app}",
            &Context {
                app: "Zoom".into(),
                ..Default::default()
            },
            "",
            None,
        );
        assert!(r.contains("\"properties\"") && r.ends_with("Zoom"));
    }
}
