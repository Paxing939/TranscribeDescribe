//! Overlay windows (§5.4, §11 п. 13, п. 27): created on demand, closed after use.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const MARGIN: f64 = 16.0;
const GAP: f64 = 10.0;

pub struct Overlays {
    app: AppHandle,
    /// label → (view payload, height) in creation order.
    open: Mutex<BTreeMap<String, (Value, f64, u64)>>,
    seq: Mutex<u64>,
    pub position: Mutex<String>,
    pub exclude_from_capture: Mutex<bool>,
}

impl Overlays {
    pub fn new(app: AppHandle, position: String, exclude: bool) -> Self {
        Overlays {
            app,
            open: Mutex::new(BTreeMap::new()),
            seq: Mutex::new(0),
            position: Mutex::new(position),
            exclude_from_capture: Mutex::new(exclude),
        }
    }

    pub fn data_for(&self, label: &str) -> Option<Value> {
        self.open
            .lock()
            .unwrap()
            .get(label)
            .map(|(v, _, _)| v.clone())
    }

    pub fn is_open(&self, label: &str) -> bool {
        self.open.lock().unwrap().contains_key(label)
    }

    /// Work area of the primary monitor in logical pixels: (x, y, w, h).
    fn work_area(&self) -> (f64, f64, f64, f64) {
        let Ok(Some(m)) = self.app.primary_monitor() else {
            return (0.0, 0.0, 1280.0, 720.0);
        };
        let s = m.scale_factor();
        let wa = m.work_area();
        (
            wa.position.x as f64 / s,
            wa.position.y as f64 / s,
            wa.size.width as f64 / s,
            wa.size.height as f64 / s,
        )
    }

    /// Top-left of a window of `w × h` stacked after the windows opened before it.
    fn slot(&self, label: &str, w: f64, h: f64) -> (f64, f64) {
        let (ax, ay, aw, ah) = self.work_area();
        let open = self.open.lock().unwrap();
        let mut order: Vec<(&String, &(Value, f64, u64))> =
            open.iter().filter(|(l, _)| l.as_str() != label).collect();
        order.sort_by_key(|(_, v)| v.2);
        let offset: f64 = order.iter().map(|(_, v)| v.1 + GAP).sum();
        let pos = self.position.lock().unwrap().clone();
        let x = if pos.ends_with("left") {
            ax + MARGIN
        } else {
            ax + aw - w - MARGIN
        };
        let y = if pos.starts_with("top") {
            ay + MARGIN + offset
        } else {
            ay + ah - h - MARGIN - offset
        };
        (x, y)
    }

    /// Show (or replace) an overlay. `focus` only for windows with an input field (F-43).
    pub fn show(&self, label: &str, view: &str, payload: Value, size: (f64, f64), focus: bool) {
        if let Some(w) = self.app.get_webview_window(label) {
            let _ = w.close();
        }
        let (x, y) = self.slot(label, size.0, size.1);
        let seq = {
            let mut s = self.seq.lock().unwrap();
            *s += 1;
            *s
        };
        self.open
            .lock()
            .unwrap()
            .insert(label.to_string(), (payload, size.1, seq));
        let exclude = *self.exclude_from_capture.lock().unwrap();
        let url = WebviewUrl::App(format!("index.html#{view}").into());
        let built = WebviewWindowBuilder::new(&self.app, label, url)
            .title("CallScribe")
            .inner_size(size.0, size.1)
            .position(x, y)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(focus)
            .focusable(focus)
            .content_protected(exclude)
            .visible(true)
            .build();
        match built {
            Ok(w) => {
                let app = self.app.clone();
                let l = label.to_string();
                w.on_window_event(move |e| {
                    if let tauri::WindowEvent::Destroyed = e
                        && let Some(o) = app.try_state::<std::sync::Arc<Overlays>>()
                    {
                        o.open.lock().unwrap().remove(&l);
                    }
                });
            }
            Err(e) => {
                tracing::error!("не удалось открыть окно {label}: {e}");
                self.open.lock().unwrap().remove(label);
            }
        }
    }

    /// New payload for an open overlay.
    pub fn update(&self, label: &str, payload: Value) {
        if let Some(entry) = self.open.lock().unwrap().get_mut(label) {
            entry.0 = payload.clone();
        } else {
            return;
        }
        let _ = self.app.emit_to(label, "view-update", payload);
    }

    pub fn close(&self, label: &str) {
        self.open.lock().unwrap().remove(label);
        if let Some(w) = self.app.get_webview_window(label) {
            let _ = w.close();
        }
    }
}
