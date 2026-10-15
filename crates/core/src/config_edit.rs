//! Edits of `config.toml` from the app that keep the user's comments (F-17, §3).

use std::path::Path;

use anyhow::{Context, Result};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::config::RuleMode;

fn mode_str(m: RuleMode) -> &'static str {
    match m {
        RuleMode::Auto => "auto",
        RuleMode::Ask => "ask",
        RuleMode::RecordThenAsk => "record_then_ask",
        RuleMode::Ignore => "ignore",
    }
}

/// A batch of key edits: `(path, Some(value))` sets, `(path, None)` removes. Comments elsewhere survive.
pub type Edits = Vec<(Vec<String>, Option<toml_edit::Value>)>;

pub fn apply_edits(text: &str, edits: &Edits) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("config.toml не разбирается")?;
    for (path, val) in edits {
        let (last, parents) = path.split_last().context("пустой путь")?;
        let mut table: &mut Table = doc.as_table_mut();
        for p in parents {
            if !table.contains_key(p) {
                let mut t = Table::new();
                t.set_implicit(false);
                table.insert(p, Item::Table(t));
            }
            table = match table.get_mut(p) {
                Some(Item::Table(t)) => t,
                _ => anyhow::bail!("{p} — не таблица"),
            };
        }
        match val {
            Some(v) => {
                // Keep the existing trailing comment of the key, if any.
                let decor = table.get(last).and_then(|i| i.as_value()).map(|v| v.decor().clone());
                let mut v = v.clone();
                if let Some(d) = decor {
                    *v.decor_mut() = d;
                }
                table.insert(last, Item::Value(v));
            }
            None => {
                table.remove(last);
            }
        }
    }
    Ok(doc.to_string())
}

pub fn apply_edits_in_file(path: &Path, edits: &Edits) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| crate::config::DEFAULT_CONFIG_TOML.to_string());
    let new = apply_edits(text.trim_start_matches('\u{feff}'), edits)?;
    crate::config::Config::from_toml_str(&new)?;
    crate::pipeline::meta::write_text_atomic(path, &new)
}

/// Set `mode` of the rule called `rule_name`; if there is no such rule, append one for `process`.
pub fn set_rule_mode(text: &str, rule_name: &str, process: &str, mode: RuleMode) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("config.toml не разбирается")?;
    let detection = doc
        .entry("detection")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("[detection] — не таблица")?;
    let rules = detection
        .entry("rules")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("detection.rules — не массив таблиц")?;

    if let Some(t) = rules
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(rule_name))
    {
        t["mode"] = value(mode_str(mode));
    } else {
        let mut t = Table::new();
        t["name"] = value(rule_name);
        let mut arr = toml_edit::Array::new();
        arr.push(process);
        t["process"] = value(arr);
        t["mode"] = value(mode_str(mode));
        // Appended last: an unknown app never matches the browser globs above it.
        rules.push(t);
    }
    Ok(doc.to_string())
}

pub fn set_rule_mode_in_file(
    path: &Path,
    rule_name: &str,
    process: &str,
    mode: RuleMode,
) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let new = set_rule_mode(&text, rule_name, process, mode)?;
    // Validate before writing.
    crate::config::Config::from_toml_str(&new)?;
    crate::pipeline::meta::write_text_atomic(path, &new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DEFAULT_CONFIG_TOML};

    #[test]
    fn edit_keeps_comments() {
        let out = set_rule_mode(
            DEFAULT_CONFIG_TOML,
            "Telegram",
            "Telegram.exe",
            RuleMode::Auto,
        )
        .unwrap();
        assert!(out.contains("# не реагировать на голосовые сообщения"));
        let c = Config::from_toml_str(&out).unwrap();
        let tg = c
            .detection
            .rules
            .iter()
            .find(|r| r.name == "Telegram")
            .unwrap();
        assert_eq!(tg.mode, Some(RuleMode::Auto));
    }

    #[test]
    fn generic_edits_keep_comments() {
        let edits: Edits = vec![
            (vec!["general".into(), "user_name".into()], Some("Пётр".into())),
            (vec!["llm".into(), "local_fallback".into()], Some({
                let mut t = toml_edit::InlineTable::new();
                t.insert("provider", "ollama".into());
                toml_edit::Value::InlineTable(t)
            })),
            (vec!["llm".into(), "api_key".into()], None),
        ];
        let out = apply_edits(DEFAULT_CONFIG_TOML, &edits).unwrap();
        assert!(out.contains("user_name = \"Пётр\""));
        assert!(out.contains("# подпись для «Я» в саммари"), "trailing comment kept");
        let c = Config::from_toml_str(&out).unwrap();
        assert_eq!(c.general.user_name, "Пётр");
        assert!(c.llm.local_fallback.is_some());
    }

    #[test]
    fn unknown_app_gets_new_rule() {
        let out = set_rule_mode(
            DEFAULT_CONFIG_TOML,
            "SomeVoip",
            "SomeVoip.exe",
            RuleMode::Ignore,
        )
        .unwrap();
        let c = Config::from_toml_str(&out).unwrap();
        let r = c.detection.rules.last().unwrap();
        assert_eq!(
            (r.name.as_str(), r.mode),
            ("SomeVoip", Some(RuleMode::Ignore))
        );
        assert_eq!(c.detection.rules.len(), 11);
    }
}
