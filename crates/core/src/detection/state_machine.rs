//! Call state machine (§4.3). Pure logic: time comes in as seconds (`now`), side effects go out as `Action`s.

use crate::config::{PromptTimeoutAction, RecordMode, RuleMode};
use crate::detection::rules::AppMatch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Auto,
    Prompt,
    RecordThenAsk,
    Manual,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Auto => "auto",
            Trigger::Prompt => "prompt",
            Trigger::RecordThenAsk => "record_then_ask",
            Trigger::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    SignalLost,
    Silence,
    MaxDuration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub app: Option<AppMatch>,
    pub trigger: Trigger,
    pub mode: RecordMode,
    pub screen: u32,
    pub cloud_allowed: bool,
    pub started_at: f64,
    /// When the session may next be asked about max duration.
    pub max_deadline: f64,
    /// `record_then_ask`: waiting for «Оставить?».
    pub keep_pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Detected {
        app: AppMatch,
        deadline: f64,
    },
    Recording(Session),
    EndSuspected {
        session: Session,
        reason: EndReason,
        deadline: f64,
    },
    Stopping(Session),
    Suppressed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Debounced start of a call signal.
    SignalStarted(AppMatch),
    /// Debounced end of all call signals.
    SignalLost,
    /// A signal is present again (not debounced).
    SignalReturned,
    UserRecord {
        mode: RecordMode,
        screen: u32,
        cloud_allowed: bool,
    },
    UserNotNow,
    KeepAnswer(bool),
    ManualStart {
        mode: RecordMode,
        screen: u32,
    },
    ManualStop,
    SilenceTimeout,
    UserContinue,
    UserStopNow,
    StopCompleted {
        signal_present: bool,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ShowStartPrompt { app: AppMatch, deadline: f64 },
    ShowKeepPrompt,
    ClosePrompt,
    StartRecording(Session),
    StopRecording { discard: bool },
    ShowEndPrompt { reason: EndReason, deadline: f64 },
    CloseEndPrompt,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub prompt_timeout_sec: f64,
    pub prompt_timeout_action: PromptTimeoutAction,
    pub countdown_sec: f64,
    pub max_duration_sec: f64,
    pub default_mode: RecordMode,
    pub default_screen: u32,
}

pub struct CallStateMachine {
    pub state: State,
    pub settings: Settings,
}

impl CallStateMachine {
    pub fn new(settings: Settings) -> Self {
        CallStateMachine {
            state: State::Idle,
            settings,
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, State::Recording(_) | State::EndSuspected { .. })
    }

    fn session(
        &self,
        app: Option<AppMatch>,
        trigger: Trigger,
        mode: RecordMode,
        screen: u32,
        cloud: bool,
        now: f64,
    ) -> Session {
        Session {
            app,
            trigger,
            mode,
            screen,
            cloud_allowed: cloud,
            started_at: now,
            max_deadline: now + self.settings.max_duration_sec,
            keep_pending: trigger == Trigger::RecordThenAsk,
        }
    }

    fn default_mode_for(&self, app: &AppMatch) -> (RecordMode, u32) {
        (
            app.record_mode.unwrap_or(self.settings.default_mode),
            app.screen.unwrap_or(self.settings.default_screen),
        )
    }

    pub fn handle(&mut self, ev: Event, now: f64) -> Vec<Action> {
        use State::*;
        let mut out = Vec::new();
        let state = std::mem::replace(&mut self.state, Idle);
        self.state = match (state, ev) {
            // ---- Idle ----
            (Idle, Event::SignalStarted(app)) => match app.mode {
                RuleMode::Ignore => Idle,
                RuleMode::Auto => {
                    let (m, s) = self.default_mode_for(&app);
                    let sess = self.session(Some(app), Trigger::Auto, m, s, true, now);
                    out.push(Action::StartRecording(sess.clone()));
                    Recording(sess)
                }
                RuleMode::RecordThenAsk => {
                    let (m, s) = self.default_mode_for(&app);
                    let sess = self.session(Some(app), Trigger::RecordThenAsk, m, s, true, now);
                    out.push(Action::StartRecording(sess.clone()));
                    out.push(Action::ShowKeepPrompt);
                    Recording(sess)
                }
                RuleMode::Ask => {
                    let deadline = now + self.settings.prompt_timeout_sec;
                    out.push(Action::ShowStartPrompt {
                        app: app.clone(),
                        deadline,
                    });
                    Detected { app, deadline }
                }
            },
            (Idle | Suppressed, Event::ManualStart { mode, screen }) => {
                let sess = self.session(None, Trigger::Manual, mode, screen, true, now);
                out.push(Action::StartRecording(sess.clone()));
                Recording(sess)
            }

            // ---- Detected ----
            (
                Detected { app, .. },
                Event::UserRecord {
                    mode,
                    screen,
                    cloud_allowed,
                },
            ) => {
                out.push(Action::ClosePrompt);
                let sess =
                    self.session(Some(app), Trigger::Prompt, mode, screen, cloud_allowed, now);
                out.push(Action::StartRecording(sess.clone()));
                Recording(sess)
            }
            (Detected { app, .. }, Event::ManualStart { mode, screen }) => {
                out.push(Action::ClosePrompt);
                let sess = self.session(Some(app), Trigger::Manual, mode, screen, true, now);
                out.push(Action::StartRecording(sess.clone()));
                Recording(sess)
            }
            (Detected { .. }, Event::UserNotNow) => {
                out.push(Action::ClosePrompt);
                Suppressed
            }
            (Detected { .. }, Event::SignalLost) => {
                out.push(Action::ClosePrompt);
                Idle
            }
            (Detected { app, deadline }, Event::Tick) if now >= deadline => {
                out.push(Action::ClosePrompt);
                match self.settings.prompt_timeout_action {
                    PromptTimeoutAction::Ignore => Suppressed,
                    PromptTimeoutAction::Record => {
                        let (m, s) = self.default_mode_for(&app);
                        let sess = self.session(Some(app), Trigger::Prompt, m, s, true, now);
                        out.push(Action::StartRecording(sess.clone()));
                        Recording(sess)
                    }
                }
            }

            // ---- Suppressed ----
            (Suppressed, Event::SignalLost) => Idle,

            // ---- Recording ----
            (Recording(mut s), Event::KeepAnswer(keep)) if s.keep_pending => {
                out.push(Action::ClosePrompt);
                if keep {
                    s.keep_pending = false;
                    Recording(s)
                } else {
                    out.push(Action::StopRecording { discard: true });
                    Stopping(s)
                }
            }
            (Recording(s), Event::SignalLost) if s.trigger != Trigger::Manual => {
                self.suspect(s, EndReason::SignalLost, now, &mut out)
            }
            (Recording(s), Event::SilenceTimeout) => {
                self.suspect(s, EndReason::Silence, now, &mut out)
            }
            (Recording(s), Event::Tick) if now >= s.max_deadline => {
                self.suspect(s, EndReason::MaxDuration, now, &mut out)
            }
            (Recording(s), Event::ManualStop) => {
                if s.keep_pending {
                    out.push(Action::ClosePrompt);
                }
                out.push(Action::StopRecording { discard: false });
                Stopping(s)
            }

            // ---- EndSuspected ----
            (
                EndSuspected {
                    mut session,
                    reason,
                    ..
                },
                Event::UserContinue,
            ) => {
                out.push(Action::CloseEndPrompt);
                if reason == EndReason::MaxDuration {
                    session.max_deadline = now + self.settings.max_duration_sec;
                }
                Recording(session)
            }
            (
                EndSuspected {
                    session,
                    reason: EndReason::SignalLost,
                    ..
                },
                Event::SignalReturned,
            ) => {
                out.push(Action::CloseEndPrompt);
                Recording(session)
            }
            (EndSuspected { session, .. }, Event::UserStopNow | Event::ManualStop) => {
                out.push(Action::CloseEndPrompt);
                out.push(Action::StopRecording { discard: false });
                Stopping(session)
            }
            (
                EndSuspected {
                    session, deadline, ..
                },
                Event::Tick,
            ) if now >= deadline => {
                out.push(Action::CloseEndPrompt);
                out.push(Action::StopRecording { discard: false });
                Stopping(session)
            }

            // ---- Stopping ----
            (Stopping(_), Event::StopCompleted { signal_present }) => {
                if signal_present {
                    Suppressed
                } else {
                    Idle
                }
            }

            (s, _) => s,
        };
        out
    }

    fn suspect(
        &self,
        session: Session,
        reason: EndReason,
        now: f64,
        out: &mut Vec<Action>,
    ) -> State {
        let deadline = now + self.settings.countdown_sec;
        out.push(Action::ShowEndPrompt { reason, deadline });
        State::EndSuspected {
            session,
            reason,
            deadline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            prompt_timeout_sec: 30.0,
            prompt_timeout_action: PromptTimeoutAction::Ignore,
            countdown_sec: 30.0,
            max_duration_sec: 3600.0,
            default_mode: RecordMode::Audio,
            default_screen: 1,
        }
    }
    fn app(mode: RuleMode) -> AppMatch {
        AppMatch {
            rule: "Zoom".into(),
            rule_index: Some(0),
            tag: "zoom".into(),
            process: "Zoom.exe".into(),
            window_title: None,
            mode,
            start_debounce_sec: None,
            record_mode: None,
            screen: None,
        }
    }
    fn starts(actions: &[Action]) -> bool {
        actions
            .iter()
            .any(|a| matches!(a, Action::StartRecording(_)))
    }

    #[test]
    fn ask_then_record_then_end_by_countdown() {
        let mut m = CallStateMachine::new(settings());
        let a = m.handle(Event::SignalStarted(app(RuleMode::Ask)), 0.0);
        assert!(matches!(a[0], Action::ShowStartPrompt { .. }));
        let a = m.handle(
            Event::UserRecord {
                mode: RecordMode::Screen,
                screen: 2,
                cloud_allowed: false,
            },
            1.0,
        );
        assert!(starts(&a));
        let State::Recording(s) = &m.state else {
            panic!()
        };
        assert_eq!(
            (s.mode, s.screen, s.cloud_allowed),
            (RecordMode::Screen, 2, false)
        );
        m.handle(Event::SignalLost, 100.0);
        assert!(matches!(m.state, State::EndSuspected { .. }));
        assert!(m.handle(Event::Tick, 110.0).is_empty());
        let a = m.handle(Event::Tick, 131.0);
        assert!(a.contains(&Action::StopRecording { discard: false }));
        m.handle(
            Event::StopCompleted {
                signal_present: false,
            },
            132.0,
        );
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn not_now_suppresses_until_signal_lost() {
        let mut m = CallStateMachine::new(settings());
        m.handle(Event::SignalStarted(app(RuleMode::Ask)), 0.0);
        m.handle(Event::UserNotNow, 1.0);
        assert_eq!(m.state, State::Suppressed);
        assert!(
            m.handle(Event::SignalStarted(app(RuleMode::Ask)), 5.0)
                .is_empty()
        );
        m.handle(Event::SignalLost, 50.0);
        assert_eq!(m.state, State::Idle);
    }

    #[test]
    fn prompt_timeout_ignore_and_record() {
        let mut m = CallStateMachine::new(settings());
        m.handle(Event::SignalStarted(app(RuleMode::Ask)), 0.0);
        m.handle(Event::Tick, 31.0);
        assert_eq!(m.state, State::Suppressed);

        let mut s = settings();
        s.prompt_timeout_action = PromptTimeoutAction::Record;
        let mut m = CallStateMachine::new(s);
        m.handle(Event::SignalStarted(app(RuleMode::Ask)), 0.0);
        assert!(starts(&m.handle(Event::Tick, 31.0)));
    }

    #[test]
    fn auto_mode_records_immediately() {
        let mut m = CallStateMachine::new(settings());
        assert!(starts(
            &m.handle(Event::SignalStarted(app(RuleMode::Auto)), 0.0)
        ));
    }

    #[test]
    fn record_then_ask_discard() {
        let mut m = CallStateMachine::new(settings());
        let a = m.handle(Event::SignalStarted(app(RuleMode::RecordThenAsk)), 0.0);
        assert!(starts(&a) && a.contains(&Action::ShowKeepPrompt));
        let a = m.handle(Event::KeepAnswer(false), 3.0);
        assert!(a.contains(&Action::StopRecording { discard: true }));
    }

    #[test]
    fn continue_and_signal_return() {
        let mut m = CallStateMachine::new(settings());
        m.handle(Event::SignalStarted(app(RuleMode::Auto)), 0.0);
        m.handle(Event::SilenceTimeout, 10.0);
        m.handle(Event::UserContinue, 11.0);
        assert!(matches!(m.state, State::Recording(_)));
        m.handle(Event::SignalLost, 20.0);
        m.handle(Event::SignalReturned, 21.0);
        assert!(matches!(m.state, State::Recording(_)));
    }

    #[test]
    fn manual_session_ignores_signal_loss_and_stop_mid_call_suppresses() {
        let mut m = CallStateMachine::new(settings());
        m.handle(
            Event::ManualStart {
                mode: RecordMode::Audio,
                screen: 1,
            },
            0.0,
        );
        m.handle(Event::SignalLost, 5.0);
        assert!(matches!(m.state, State::Recording(_)));
        m.handle(Event::ManualStop, 6.0);
        m.handle(
            Event::StopCompleted {
                signal_present: true,
            },
            7.0,
        );
        assert_eq!(m.state, State::Suppressed);
    }

    #[test]
    fn max_duration_asks_and_continue_extends() {
        let mut m = CallStateMachine::new(settings());
        m.handle(Event::SignalStarted(app(RuleMode::Auto)), 0.0);
        m.handle(Event::Tick, 3601.0);
        assert!(matches!(
            m.state,
            State::EndSuspected {
                reason: EndReason::MaxDuration,
                ..
            }
        ));
        m.handle(Event::UserContinue, 3602.0);
        m.handle(Event::Tick, 3700.0);
        assert!(matches!(m.state, State::Recording(_)));
    }
}
