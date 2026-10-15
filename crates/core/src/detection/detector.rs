//! Signal merging and debounce (F-15), silence detection (F-16).

use crate::detection::rules::AppMatch;

/// Result of one poll after rule matching.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Source `window` (F-11).
    pub window: Option<AppMatch>,
    /// Source `mic` (F-10).
    pub mic: Option<AppMatch>,
}

impl Snapshot {
    pub fn present(&self) -> bool {
        self.window.is_some() || self.mic.is_some()
    }

    /// The window source knows the meeting better (title), so it wins.
    pub fn best(&self) -> Option<&AppMatch> {
        self.window.as_ref().or(self.mic.as_ref())
    }

    pub fn detected_by(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.window.is_some() {
            v.push("window");
        }
        if self.mic.is_some() {
            v.push("mic");
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SignalEvent {
    Started {
        app: AppMatch,
        detected_by: Vec<&'static str>,
    },
    Lost,
    Returned,
}

#[derive(Debug)]
pub struct Debouncer {
    pub start_debounce_sec: f64,
    pub end_debounce_sec: f64,
    present_since: Option<f64>,
    absent_since: Option<f64>,
    active: bool,
    returned_sent: bool,
    lost_once: bool,
}

impl Debouncer {
    pub fn new(start_debounce_sec: f64, end_debounce_sec: f64) -> Self {
        Debouncer {
            start_debounce_sec,
            end_debounce_sec,
            present_since: None,
            absent_since: None,
            active: false,
            returned_sent: false,
            lost_once: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn update(&mut self, snap: &Snapshot, now: f64) -> Vec<SignalEvent> {
        let mut out = Vec::new();
        if let Some(app) = snap.best() {
            self.absent_since = None;
            if self.active {
                return out;
            }
            if self.lost_once && !self.returned_sent {
                self.returned_sent = true;
                out.push(SignalEvent::Returned);
            }
            let since = *self.present_since.get_or_insert(now);
            let need = app.start_debounce_sec.unwrap_or(self.start_debounce_sec);
            if now - since >= need {
                self.active = true;
                out.push(SignalEvent::Started {
                    app: app.clone(),
                    detected_by: snap.detected_by(),
                });
            }
        } else {
            self.present_since = None;
            self.returned_sent = false;
            if !self.active {
                return out;
            }
            let since = *self.absent_since.get_or_insert(now);
            if now - since >= self.end_debounce_sec {
                self.active = false;
                self.lost_once = true;
                self.absent_since = None;
                out.push(SignalEvent::Lost);
            }
        }
        out
    }
}

/// Both inputs below the threshold for `timeout` seconds → one `true` (F-16).
#[derive(Debug)]
pub struct SilenceMonitor {
    pub threshold_db: f64,
    pub timeout_sec: f64,
    last_loud: Option<f64>,
    fired: bool,
}

impl SilenceMonitor {
    pub fn new(threshold_db: f64, timeout_sec: f64) -> Self {
        SilenceMonitor {
            threshold_db,
            timeout_sec,
            last_loud: None,
            fired: false,
        }
    }

    pub fn reset(&mut self, now: f64) {
        self.last_loud = Some(now);
        self.fired = false;
    }

    /// `levels_db` — per-second peak of each watched input.
    pub fn update(&mut self, levels_db: &[f64], now: f64) -> bool {
        let last = *self.last_loud.get_or_insert(now);
        if levels_db.iter().any(|&db| db > self.threshold_db) {
            self.last_loud = Some(now);
            self.fired = false;
            return false;
        }
        if !self.fired && now - last >= self.timeout_sec {
            self.fired = true;
            return true;
        }
        false
    }
}

/// Linear amplitude (0..1) → dBFS.
pub fn amplitude_to_db(a: f64) -> f64 {
    if a <= 1e-9 { -120.0 } else { 20.0 * a.log10() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuleMode;

    fn app(debounce: Option<f64>) -> AppMatch {
        AppMatch {
            rule: "Telegram".into(),
            rule_index: Some(3),
            tag: "telegram".into(),
            process: "Telegram.exe".into(),
            window_title: None,
            mode: RuleMode::Ask,
            start_debounce_sec: debounce,
            record_mode: None,
            screen: None,
        }
    }
    fn mic(a: AppMatch) -> Snapshot {
        Snapshot {
            window: None,
            mic: Some(a),
        }
    }

    #[test]
    fn start_after_debounce_and_rule_override() {
        let mut d = Debouncer::new(3.0, 10.0);
        assert!(d.update(&mic(app(None)), 0.0).is_empty());
        assert!(d.update(&mic(app(None)), 2.0).is_empty());
        assert!(matches!(
            d.update(&mic(app(None)), 3.0)[0],
            SignalEvent::Started { .. }
        ));

        // Telegram voice message (5 s) with start_debounce 10 → nothing.
        let mut d = Debouncer::new(3.0, 10.0);
        for t in 0..6 {
            assert!(d.update(&mic(app(Some(10.0))), t as f64).is_empty());
        }
        assert!(d.update(&Snapshot::default(), 6.0).is_empty());
    }

    #[test]
    fn short_gap_does_not_end_call() {
        let mut d = Debouncer::new(0.0, 10.0);
        d.update(&mic(app(None)), 0.0);
        assert!(d.update(&Snapshot::default(), 1.0).is_empty());
        assert!(d.update(&Snapshot::default(), 5.0).is_empty());
        assert!(d.update(&mic(app(None)), 6.0).is_empty());
        assert!(d.update(&Snapshot::default(), 7.0).is_empty());
        assert_eq!(
            d.update(&Snapshot::default(), 17.0),
            vec![SignalEvent::Lost]
        );
        assert_eq!(d.update(&mic(app(None)), 18.0)[0], SignalEvent::Returned);
    }

    #[test]
    fn silence() {
        let mut s = SilenceMonitor::new(-50.0, 180.0);
        assert!(!s.update(&[-60.0, -70.0], 0.0));
        assert!(!s.update(&[-60.0, -70.0], 100.0));
        assert!(!s.update(&[-20.0, -70.0], 120.0));
        assert!(!s.update(&[-60.0, -70.0], 299.0));
        assert!(s.update(&[-60.0, -70.0], 300.0));
        assert!(!s.update(&[-60.0, -70.0], 301.0), "fires once");
    }
}
