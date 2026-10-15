//! Посекундная агрегация `InputVolumeMeters` (§11 п. 14).

use callscribe_core::detection::detector::amplitude_to_db;

/// Копит пиковую амплитуду наблюдаемых входов за окно (секунду).
#[derive(Debug, Clone, Default)]
pub struct LevelAggregator {
    mic: Option<String>,
    desktop: Option<String>,
    mic_peak: f32,
    desktop_peak: f32,
    seen: bool,
}

impl LevelAggregator {
    pub fn new(mic: Option<String>, desktop: Option<String>) -> Self {
        Self {
            mic,
            desktop,
            ..Self::default()
        }
    }

    /// Одно событие `InputVolumeMeters`: пары (имя входа, пик в Mul).
    /// Входа нет в событии — он неактивен, то есть тишина.
    pub fn feed<'a>(&mut self, inputs: impl IntoIterator<Item = (&'a str, f32)>) {
        self.seen = true;
        for (name, peak) in inputs {
            if self.mic.as_deref() == Some(name) {
                self.mic_peak = self.mic_peak.max(peak);
            }
            if self.desktop.as_deref() == Some(name) {
                self.desktop_peak = self.desktop_peak.max(peak);
            }
        }
    }

    /// Итог окна в дБ `(mic_db, desktop_db)` и сброс. `None`, если за окно не
    /// пришло ни одного события (метры выключены — «тишину» не выдумываем).
    pub fn flush(&mut self) -> Option<(f64, f64)> {
        if !self.seen {
            return None;
        }
        let out = (
            amplitude_to_db(f64::from(self.mic_peak)),
            amplitude_to_db(f64::from(self.desktop_peak)),
        );
        self.mic_peak = 0.0;
        self.desktop_peak = 0.0;
        self.seen = false;
        Some(out)
    }
}

/// Максимум пика (индекс 1 в `[magnitude, peak, input_peak]`) по каналам.
pub fn peak_of(levels: &[[f32; 3]]) -> f32 {
    levels.iter().map(|l| l[1]).fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg() -> LevelAggregator {
        LevelAggregator::new(Some("Mic".into()), Some("Desktop".into()))
    }

    #[test]
    fn takes_peak_over_window_and_converts_to_db() {
        let mut a = agg();
        a.feed([("Mic", 0.01), ("Desktop", 0.5), ("Other", 1.0)]);
        a.feed([("Mic", 0.1), ("Desktop", 0.1)]);
        let (m, d) = a.flush().unwrap();
        assert!((m - (-20.0)).abs() < 1e-6, "{m}");
        assert!((d - (-6.0206)).abs() < 1e-3, "{d}");
    }

    #[test]
    fn flush_resets_and_is_none_without_events() {
        let mut a = agg();
        assert!(a.flush().is_none());
        a.feed([("Mic", 0.1)]);
        assert!(a.flush().is_some());
        assert!(a.flush().is_none());
    }

    #[test]
    fn absent_input_is_silence() {
        let mut a = agg();
        a.feed([("Mic", 0.1)]);
        let (_, d) = a.flush().unwrap();
        assert_eq!(d, -120.0);
    }

    #[test]
    fn peak_over_channels() {
        assert_eq!(peak_of(&[[0.0, 0.2, 0.0], [0.0, 0.4, 0.0]]), 0.4);
        assert_eq!(peak_of(&[]), 0.0);
    }
}
