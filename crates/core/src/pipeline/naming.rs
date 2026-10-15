//! Names of call folders and files (F-80…F-82).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};

use crate::config::Naming;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Make a string safe as a Windows file name component (F-81).
pub fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Lowercase, spaces → `-`, only letters/digits/`-`/`_`; Cyrillic kept unless `transliterate`.
pub fn slug(s: &str, transliterate: bool) -> String {
    let src = if transliterate {
        deunicode::deunicode(s)
    } else {
        s.to_string()
    };
    let mut out = String::new();
    let mut dash = false;
    for c in src.to_lowercase().chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Cut to `max` characters on a word boundary when possible.
pub fn truncate_title(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind([' ', '-']) {
        Some(i) if i > max / 2 => cut[..i].to_string(),
        _ => cut,
    }
}

pub struct NameParts<'a> {
    pub started_at: DateTime<FixedOffset>,
    pub app_tag: &'a str,
    pub title: Option<&'a str>,
}

/// Folder/file base name. Without a title: `{date}_{time}_{app}` (F-80).
pub fn base_name(cfg: &Naming, parts: &NameParts) -> String {
    let date = parts.started_at.format(&cfg.date_format).to_string();
    let time = parts.started_at.format(&cfg.time_format).to_string();
    let app = slug(parts.app_tag, cfg.transliterate);
    let name = match parts.title.map(str::trim).filter(|t| !t.is_empty()) {
        None => format!("{date}_{time}_{app}"),
        Some(title) => {
            let title = truncate_title(title, cfg.max_title_len);
            let title = if cfg.slugify {
                slug(&title, cfg.transliterate)
            } else {
                sanitize(&title)
            };
            cfg.template
                .replace("{date}", &date)
                .replace("{time}", &time)
                .replace("{app}", &app)
                .replace("{title}", &title)
        }
    };
    let name = sanitize(&name);
    name.trim_matches(['_', '-']).to_string()
}

/// `dir/name`, `dir/name-2`, `dir/name-3`… — the first that does not exist (F-81).
pub fn unique_path(dir: &Path, name: &str, ext: Option<&str>, except: Option<&Path>) -> PathBuf {
    let make = |n: &str| match ext {
        Some(e) => dir.join(format!("{n}.{e}")),
        None => dir.join(n),
    };
    let first = make(name);
    if !first.exists() || except.is_some_and(|e| e == first) {
        return first;
    }
    (2..)
        .map(|i| make(&format!("{name}-{i}")))
        .find(|p| !p.exists() || except.is_some_and(|e| e == p))
        .unwrap()
}

/// Rename with retries: the file may be held by OBS, a player or an antivirus (F-82).
pub fn rename_with_retry(from: &Path, to: &Path, total: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    let mut delay = Duration::from_millis(200);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if start.elapsed() < total => {
                tracing::debug!(
                    "rename {} → {} failed: {e}; retry",
                    from.display(),
                    to.display()
                );
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(10));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "не удалось переименовать {} → {}",
                        from.display(),
                        to.display()
                    )
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn ts() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-09T14:30:12+03:00").unwrap()
    }

    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize("a<b>c:d\"e/f\\g|h?i*j"), "a b c d e f g h i j");
        assert_eq!(sanitize("Итоги. "), "Итоги");
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("COM1.txt"), "_COM1.txt");
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slug("Обсуждение CI-пайплайна", false),
            "обсуждение-ci-пайплайна"
        );
        assert_eq!(slug("Обсуждение", true), "obsuzhdenie");
        assert_eq!(slug("Google Meet", false), "google-meet");
    }

    #[test]
    fn names() {
        let c = Config::default().naming;
        let p = NameParts {
            started_at: ts(),
            app_tag: "zoom",
            title: None,
        };
        assert_eq!(base_name(&c, &p), "2026-10-09_14-30_zoom");
        let p = NameParts {
            started_at: ts(),
            app_tag: "zoom",
            title: Some("Обсуждение CI-пайплайна"),
        };
        assert_eq!(
            base_name(&c, &p),
            "2026-10-09_14-30_zoom_обсуждение-ci-пайплайна"
        );
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_title("один два три четыре", 12), "один два");
    }

    #[test]
    fn unique() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("x")).unwrap();
        std::fs::create_dir(d.path().join("x-2")).unwrap();
        assert_eq!(unique_path(d.path(), "x", None, None), d.path().join("x-3"));
        assert_eq!(
            unique_path(d.path(), "x", None, Some(&d.path().join("x"))),
            d.path().join("x")
        );
    }
}
