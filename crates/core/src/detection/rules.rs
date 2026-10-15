//! Detection rules (F-12…F-14): process/window → rule.

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::config::{Detection, RecordMode, RuleConfig, RuleMode};

/// A process as seen by the platform layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// File name of the executable, e.g. `Telegram.exe`.
    pub exe_name: String,
    /// Package family name for packaged (MSIX) apps, e.g. `MSTeams_8wekyb3d8bbwe`.
    pub package_family: Option<String>,
}

impl ProcessInfo {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.exe_name.as_str()).chain(self.package_family.as_deref())
    }
}

/// A visible top-level window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub pid: u32,
    pub exe_name: String,
    pub title: String,
}

/// Which app a signal belongs to; written to `meta.json` as `app`.
#[derive(Debug, Clone, PartialEq)]
pub struct AppMatch {
    /// Rule name, or the exe stem for unknown apps.
    pub rule: String,
    pub rule_index: Option<usize>,
    pub tag: String,
    pub process: String,
    pub window_title: Option<String>,
    pub mode: RuleMode,
    pub start_debounce_sec: Option<f64>,
    pub record_mode: Option<RecordMode>,
    pub screen: Option<u32>,
}

#[derive(Debug)]
pub struct CompiledRule {
    pub name: String,
    process: GlobSet,
    meeting_process: GlobSet,
    meeting_window: Option<Regex>,
    meeting_ignore: Option<Regex>,
    window_title: Option<Regex>,
    pub mode: Option<RuleMode>,
    pub start_debounce_sec: Option<f64>,
    pub record_mode: Option<RecordMode>,
    pub screen: Option<u32>,
    pub tag: Option<String>,
}

#[derive(Debug)]
pub struct RuleSet {
    pub rules: Vec<CompiledRule>,
    ignore: GlobSet,
    default_mode: RuleMode,
}

fn glob_set(
    patterns: &[String],
    sets: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let expanded: Vec<&String> = match p.strip_prefix('@') {
            Some(set) => sets
                .get(set)
                .with_context(|| format!("нет набора процессов @{set}"))?
                .iter()
                .collect(),
            None => vec![p],
        };
        for p in expanded {
            b.add(
                GlobBuilder::new(p)
                    .case_insensitive(true)
                    .literal_separator(false)
                    .build()
                    .with_context(|| format!("неверный glob {p}"))?,
            );
        }
    }
    Ok(b.build()?)
}

fn opt_regex(r: &Option<String>) -> Result<Option<Regex>> {
    r.as_deref().map(Regex::new).transpose().map_err(Into::into)
}

impl RuleSet {
    pub fn compile(cfg: &Detection) -> Result<RuleSet> {
        let rules = cfg
            .rules
            .iter()
            .map(|r| CompiledRule::compile(r, cfg))
            .collect::<Result<Vec<_>>>()?;
        Ok(RuleSet {
            rules,
            ignore: glob_set(&cfg.ignore_processes, &cfg.process_sets)?,
            default_mode: cfg.default_mode,
        })
    }

    pub fn is_ignored(&self, p: &ProcessInfo) -> bool {
        p.names().any(|n| self.ignore.is_match(n))
    }

    /// Source `window` (F-11): an open meeting window or a running meeting-only process.
    pub fn match_meeting(
        &self,
        windows: &[WindowInfo],
        processes: &[ProcessInfo],
    ) -> Option<AppMatch> {
        for (i, rule) in self.rules.iter().enumerate() {
            if let Some(re) = &rule.meeting_window {
                for w in windows {
                    if !rule.process.is_match(&w.exe_name) || !re.is_match(&w.title) {
                        continue;
                    }
                    if rule
                        .meeting_ignore
                        .as_ref()
                        .is_some_and(|ig| ig.is_match(&w.title))
                    {
                        continue;
                    }
                    return Some(self.app_match(i, &w.exe_name, Some(&w.title)));
                }
            }
            if !rule.meeting_process.is_empty()
                && let Some(p) = processes
                    .iter()
                    .find(|p| p.names().any(|n| rule.meeting_process.is_match(n)))
            {
                // Prefer a window of the main app for the title hint.
                let title = windows
                    .iter()
                    .find(|w| rule.process.is_match(&w.exe_name))
                    .map(|w| w.title.as_str());
                return Some(self.app_match(i, &p.exe_name, title));
            }
        }
        None
    }

    /// Source `mic` (F-10, F-12): `chain` is the capturing process followed by its ancestors;
    /// `windows` are all visible windows (used for `window_title_regex`).
    /// Returns `None` if the process is ignored or its rule has mode `ignore`.
    pub fn match_mic(&self, chain: &[ProcessInfo], windows: &[WindowInfo]) -> Option<AppMatch> {
        let first = chain.first()?;
        if chain.iter().any(|p| self.is_ignored(p)) {
            return None;
        }
        for (i, rule) in self.rules.iter().enumerate() {
            let Some(proc_hit) = chain
                .iter()
                .find(|p| p.names().any(|n| rule.process.is_match(n)))
            else {
                continue;
            };
            let exe = &proc_hit.exe_name;
            match &rule.window_title {
                None => {
                    let title = windows
                        .iter()
                        .find(|w| w.exe_name.eq_ignore_ascii_case(exe))
                        .map(|w| w.title.as_str());
                    return self.filter_ignore(self.app_match(i, exe, title));
                }
                Some(re) => {
                    // Windows of the same app: same exe as any process in the chain (§11, п. 29).
                    let hit = windows.iter().find(|w| {
                        chain
                            .iter()
                            .any(|p| p.exe_name.eq_ignore_ascii_case(&w.exe_name))
                            && re.is_match(&w.title)
                    });
                    if let Some(w) = hit {
                        let mut m = self.app_match(i, exe, Some(&w.title));
                        if rule.tag.is_none()
                            && let Some(word) = re.find(&w.title)
                        {
                            m.tag = crate::pipeline::naming::slug(word.as_str(), false);
                        }
                        return self.filter_ignore(m);
                    }
                }
            }
        }
        // Unknown app → default_mode.
        let stem = exe_stem(&first.exe_name);
        let m = AppMatch {
            rule: stem.clone(),
            rule_index: None,
            tag: crate::pipeline::naming::slug(&stem, false),
            process: first.exe_name.clone(),
            window_title: windows
                .iter()
                .find(|w| w.pid == first.pid)
                .map(|w| w.title.clone()),
            mode: self.default_mode,
            start_debounce_sec: None,
            record_mode: None,
            screen: None,
        };
        self.filter_ignore(m)
    }

    fn filter_ignore(&self, m: AppMatch) -> Option<AppMatch> {
        (m.mode != RuleMode::Ignore).then_some(m)
    }

    fn app_match(&self, i: usize, exe: &str, title: Option<&str>) -> AppMatch {
        let r = &self.rules[i];
        AppMatch {
            rule: r.name.clone(),
            rule_index: Some(i),
            tag: r
                .tag
                .clone()
                .unwrap_or_else(|| crate::pipeline::naming::slug(&r.name, false)),
            process: exe.to_string(),
            window_title: title.map(str::to_string),
            mode: r.mode.unwrap_or(self.default_mode),
            start_debounce_sec: r.start_debounce_sec,
            record_mode: r.record_mode,
            screen: r.screen,
        }
    }
}

impl CompiledRule {
    fn compile(r: &RuleConfig, cfg: &Detection) -> Result<CompiledRule> {
        Ok(CompiledRule {
            name: r.name.clone(),
            process: glob_set(&r.process, &cfg.process_sets)
                .with_context(|| format!("правило «{}»", r.name))?,
            meeting_process: glob_set(&r.meeting_process, &cfg.process_sets)?,
            meeting_window: opt_regex(&r.meeting_window_regex)?,
            meeting_ignore: opt_regex(&r.meeting_window_ignore_regex)?,
            window_title: opt_regex(&r.window_title_regex)?,
            mode: r.mode,
            start_debounce_sec: r.start_debounce_sec,
            record_mode: r.record_mode,
            screen: r.screen,
            tag: r.tag.clone(),
        })
    }
}

/// `C:\Program Files\Foo\Bar.exe` or `Bar.exe` → `Bar`.
pub fn exe_stem(exe: &str) -> String {
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    file.strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file)
        .to_string()
}

/// Registry `NonPackaged` key name (`C:#Program Files#Foo#Bar.exe`) → `Bar.exe`.
pub fn exe_from_registry_key(key: &str) -> String {
    key.rsplit('#').next().unwrap_or(key).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn rules() -> RuleSet {
        RuleSet::compile(&Config::default().detection).unwrap()
    }
    fn proc(pid: u32, exe: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            exe_name: exe.into(),
            package_family: None,
        }
    }
    fn win(pid: u32, exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            pid,
            exe_name: exe.into(),
            title: title.into(),
        }
    }

    #[test]
    fn telegram_mic() {
        let m = rules().match_mic(&[proc(1, "Telegram.exe")], &[]).unwrap();
        assert_eq!(m.rule, "Telegram");
        assert_eq!(m.start_debounce_sec, Some(10.0));
    }

    #[test]
    fn obs_is_ignored() {
        assert!(rules().match_mic(&[proc(1, "obs64.exe")], &[]).is_none());
    }

    #[test]
    fn chrome_utility_child_with_meet_tab() {
        let chain = [proc(20, "chrome.exe"), proc(10, "chrome.exe")];
        let wins = [win(10, "chrome.exe", "Meet – abc-defg-hij - Google Chrome")];
        let m = rules().match_mic(&chain, &wins).unwrap();
        assert_eq!(m.rule, "Google Meet");
        assert_eq!(m.tag, "meet");
    }

    #[test]
    fn browser_generic_call_uses_matched_word_as_tag() {
        let wins = [win(10, "msedge.exe", "Яндекс Телемост — Microsoft Edge")];
        let m = rules().match_mic(&[proc(10, "msedge.exe")], &wins).unwrap();
        assert_eq!(m.rule, "Звонок в браузере");
        assert_eq!(m.tag, "телемост");
    }

    #[test]
    fn browser_without_known_title_falls_to_browser_rule() {
        let wins = [win(10, "chrome.exe", "YouTube - Google Chrome")];
        let m = rules().match_mic(&[proc(10, "chrome.exe")], &wins).unwrap();
        assert_eq!(m.rule, "Браузер");
        assert_eq!(m.tag, "browser");
    }

    #[test]
    fn teams_packaged_name() {
        let p = ProcessInfo {
            pid: 5,
            exe_name: "ms-teams.exe".into(),
            package_family: Some("MSTeams_8wekyb3d8bbwe".into()),
        };
        assert_eq!(rules().match_mic(&[p], &[]).unwrap().rule, "Teams");
    }

    #[test]
    fn unknown_app_gets_default_mode() {
        let m = rules().match_mic(&[proc(3, "SomeVoip.exe")], &[]).unwrap();
        assert_eq!(m.rule, "SomeVoip");
        assert_eq!(m.mode, RuleMode::Ask);
        assert_eq!(m.rule_index, None);
    }

    #[test]
    fn meeting_window_zoom_and_ignore_main_window() {
        let r = rules();
        assert!(
            r.match_meeting(&[win(1, "Zoom.exe", "Zoom Workplace")], &[])
                .is_none()
        );
        let m = r
            .match_meeting(&[win(1, "Zoom.exe", "Zoom Meeting")], &[])
            .unwrap();
        assert_eq!(m.rule, "Zoom");
    }

    #[test]
    fn teams_sections_are_not_meetings() {
        let r = rules();
        assert!(
            r.match_meeting(&[win(1, "ms-teams.exe", "Чат | Microsoft Teams")], &[])
                .is_none()
        );
        assert!(
            r.match_meeting(&[win(1, "ms-teams.exe", "Планёрка | Microsoft Teams")], &[])
                .is_some()
        );
    }

    #[test]
    fn meeting_process_triggers_window_source() {
        let m = rules()
            .match_meeting(&[], &[proc(9, "CptHost.exe")])
            .unwrap();
        assert_eq!(m.rule, "Zoom");
    }

    #[test]
    fn registry_key_to_exe() {
        assert_eq!(
            exe_from_registry_key("C:#Program Files#Telegram Desktop#Telegram.exe"),
            "Telegram.exe"
        );
        assert_eq!(exe_stem("C:\\x\\Zoom.exe"), "Zoom");
    }
}
