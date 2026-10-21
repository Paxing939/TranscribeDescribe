//! Чистые проверки настроек OBS (F-27): на входе уже прочитанные значения,
//! на выходе — русское описание проблемы или `None`.

/// Маска дорожек по номерам 1..=6 (бит 0 = дорожка 1).
#[cfg(test)]
mod device_tests {
    use crate::client::{AudioDevice, describe_audio_devices, is_virtual_device};

    #[test]
    fn virtual_default_mic_warns() {
        assert!(is_virtual_device("Headset Microphone (Oculus Virtual Audio Device)"));
        assert!(!is_virtual_device("Microphone (G435 Wireless Gaming Headset)"));
        let devs = vec![
            AudioDevice { role: "mic", input: "Mic/Aux".into(), device_id: "default".into(), label: Some("Default".into()), is_default: true },
            AudioDevice { role: "desktop", input: "Desktop Audio".into(), device_id: "{x}".into(), label: Some("Headset Earphone (G435)".into()), is_default: false },
        ];
        let (report, warn) = describe_audio_devices(&devs, Some("Headset Microphone (Oculus Virtual Audio Device)"), None);
        assert!(report[0].contains("Oculus") && report[1].contains("G435"));
        assert_eq!(warn.len(), 1);
    }
}

pub fn tracks_mask(tracks: &[u32]) -> u32 {
    tracks
        .iter()
        .filter(|t| (1..=6).contains(*t))
        .fold(0, |m, t| m | (1 << (t - 1)))
}

pub fn check_output_mode(mode: Option<&str>) -> Option<String> {
    match mode {
        Some("Advanced") => None,
        other => Some(format!(
            "Режим вывода OBS — «{}», нужен «Расширенный» (Настройки → Вывод → Режим вывода).",
            match other {
                None | Some("Simple") => "Простой",
                Some(x) => x,
            }
        )),
    }
}

/// `AdvOut/RecTracks`: должны быть включены все `required` дорожки (1-based).
pub fn check_rec_tracks(value: Option<&str>, required: &[u32]) -> Option<String> {
    let need = tracks_mask(required);
    let Some(mask) = value.and_then(|v| v.trim().parse::<u32>().ok()) else {
        return Some("Не удалось определить звуковые дорожки записи (AdvOut/RecTracks).".into());
    };
    if mask & need == need {
        return None;
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|t| mask & tracks_mask(&[**t]) == 0)
        .map(u32::to_string)
        .collect();
    Some(format!(
        "В записи OBS не включены звуковые дорожки: {} (Настройки → Вывод → Запись).",
        missing.join(", ")
    ))
}

/// Формат записи: `RecFormat2` (OBS 30+), иначе старый `RecFormat`.
pub fn check_rec_format(format2: Option<&str>, format: Option<&str>) -> Option<String> {
    let fmt = [format2, format]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty());
    match fmt {
        Some("mkv" | "hybrid_mp4") => None,
        Some(other) => Some(format!(
            "Формат записи OBS — «{other}», нужен MKV или Hybrid MP4."
        )),
        None => Some("Не удалось определить формат записи (AdvOut/RecFormat2).".into()),
    }
}

/// Дорожки входа: каждая из `required` (1-based) должна быть включена,
/// каждая из `forbidden` — выключена (иначе микрофон попадёт на дорожку
/// собеседника и наоборот).
pub fn check_input_tracks(
    input: &str,
    enabled: &[bool; 6],
    required: &[u32],
    forbidden: &[u32],
) -> Option<String> {
    let missing: Vec<String> = required
        .iter()
        .filter(|t| !(1..=6).contains(*t) || !enabled[(**t - 1) as usize])
        .map(u32::to_string)
        .collect();
    let extra: Vec<String> = forbidden
        .iter()
        .filter(|t| (1..=6).contains(*t) && !required.contains(t) && enabled[(**t - 1) as usize])
        .map(u32::to_string)
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!(
            "не отправляет звук на дорожки: {}",
            missing.join(", ")
        ));
    }
    if !extra.is_empty() {
        parts.push(format!(
            "отправляет лишний звук на дорожки: {}",
            extra.join(", ")
        ));
    }
    Some(format!(
        "Источник «{input}» {} (Расширенные настройки звука).",
        parts.join("; ")
    ))
}

/// Что поменять в дорожках входа: `Some(включить/выключить)` для нужных
/// дорожек, остальные `None`. Все `None` — менять нечего.
pub fn plan_input_tracks(
    enabled: &[bool; 6],
    required: &[u32],
    forbidden: &[u32],
) -> [Option<bool>; 6] {
    let mut out = [None; 6];
    for t in required.iter().filter(|t| (1..=6).contains(*t)) {
        let i = (*t - 1) as usize;
        if !enabled[i] {
            out[i] = Some(true);
        }
    }
    for t in forbidden.iter().filter(|t| (1..=6).contains(*t)) {
        let i = (*t - 1) as usize;
        if enabled[i] && !required.contains(t) {
            out[i] = Some(false);
        }
    }
    out
}

/// Одно изменение параметра профиля OBS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamChange {
    pub category: &'static str,
    pub name: &'static str,
    pub value: String,
    /// Описание для пользователя.
    pub note: &'static str,
}

fn change(
    category: &'static str,
    name: &'static str,
    value: &str,
    note: &'static str,
) -> ParamChange {
    ParamChange {
        category,
        name,
        value: value.to_owned(),
        note,
    }
}

/// Правки текущего профиля: режим «Расширенный», дорожки записи, MKV.
/// Дорожки только добавляются (чужие включённые не выключаем).
pub fn plan_main_profile(
    mode: Option<&str>,
    rec_tracks: Option<&str>,
    format2: Option<&str>,
    format: Option<&str>,
    required: &[u32],
) -> Vec<ParamChange> {
    let mut out = Vec::new();
    if check_output_mode(mode).is_some() {
        out.push(change(
            "Output",
            "Mode",
            "Advanced",
            "режим вывода переключён на «Расширенный»",
        ));
    }
    if check_rec_tracks(rec_tracks, required).is_some() {
        let cur = rec_tracks
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let new = (cur | tracks_mask(required)).to_string();
        out.push(change(
            "AdvOut",
            "RecTracks",
            &new,
            "в записи включены звуковые дорожки 1–3",
        ));
    }
    if check_rec_format(format2, format).is_some() {
        out.push(change(
            "AdvOut",
            "RecFormat2",
            "mkv",
            "формат записи изменён на MKV",
        ));
    }
    out
}

/// Правки профиля «только звук»: Custom FFmpeg, Matroska Audio (.mka), AAC,
/// дорожки `required`. `get(category, name)` читает текущее значение.
pub fn plan_audio_profile(
    get: &dyn Fn(&str, &str) -> Option<String>,
    required: &[u32],
) -> Vec<ParamChange> {
    let mut out = Vec::new();
    let mut want = |c: &'static str, n: &'static str, v: &str, note: &'static str| {
        if get(c, n).as_deref().unwrap_or("") != v {
            out.push(change(c, n, v, note));
        }
    };
    want("Output", "Mode", "Advanced", "режим вывода «Расширенный»");
    want(
        "AdvOut",
        "RecType",
        "FFmpeg",
        "тип записи «Пользовательский вывод (FFmpeg)»",
    );
    want("AdvOut", "FFOutputToFile", "true", "вывод FFmpeg — в файл");
    want(
        "AdvOut",
        "FFFormat",
        "matroska",
        "контейнер Matroska Audio (.mka)",
    );
    want(
        "AdvOut",
        "FFFormatMimeType",
        "audio/x-matroska",
        "контейнер Matroska Audio (.mka)",
    );
    want(
        "AdvOut",
        "FFExtension",
        "mka",
        "контейнер Matroska Audio (.mka)",
    );
    want("AdvOut", "FFVEncoderId", "0", "без видеокодера");
    want("AdvOut", "FFVEncoder", "", "без видеокодера");
    want("AdvOut", "FFAEncoderId", "86018", "аудиокодер AAC");
    want("AdvOut", "FFAEncoder", "aac", "аудиокодер AAC");
    let need = tracks_mask(required);
    let cur = get("AdvOut", "FFAudioMixes")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if cur & need != need {
        out.push(change(
            "AdvOut",
            "FFAudioMixes",
            &(cur | need).to_string(),
            "включены звуковые дорожки 1–3",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask() {
        assert_eq!(tracks_mask(&[1, 2, 3]), 7);
        assert_eq!(tracks_mask(&[1, 3]), 5);
        assert_eq!(tracks_mask(&[0, 9]), 0);
    }

    #[test]
    fn rec_tracks() {
        assert!(check_rec_tracks(Some("7"), &[1, 2, 3]).is_none());
        assert!(check_rec_tracks(Some("63"), &[1, 2, 3]).is_none());
        let m = check_rec_tracks(Some("5"), &[1, 2, 3]).unwrap();
        assert!(m.contains(": 2 "), "{m}");
        assert!(
            check_rec_tracks(Some("1"), &[1, 2, 3])
                .unwrap()
                .contains("2, 3")
        );
        assert!(check_rec_tracks(None, &[1, 2, 3]).is_some());
        assert!(check_rec_tracks(Some("abc"), &[1, 2, 3]).is_some());
    }

    #[test]
    fn output_mode() {
        assert!(check_output_mode(Some("Advanced")).is_none());
        assert!(
            check_output_mode(Some("Simple"))
                .unwrap()
                .contains("Простой")
        );
        assert!(check_output_mode(None).is_some());
    }

    #[test]
    fn format() {
        assert!(check_rec_format(Some("mkv"), None).is_none());
        assert!(check_rec_format(Some("hybrid_mp4"), Some("mp4")).is_none());
        assert!(check_rec_format(None, Some("mkv")).is_none());
        assert!(check_rec_format(Some(""), Some("mkv")).is_none());
        assert!(check_rec_format(Some("mp4"), None).unwrap().contains("mp4"));
        assert!(check_rec_format(None, None).is_some());
    }

    #[test]
    fn input_tracks() {
        let on = [true, true, false, false, false, false];
        assert!(check_input_tracks("Mic", &on, &[1, 2], &[3]).is_none());
        assert!(
            check_input_tracks("Desktop", &on, &[1, 3], &[2])
                .unwrap()
                .contains(": 3")
        );
        let all = [true; 6];
        assert!(
            check_input_tracks("Mic", &all, &[1, 2], &[3])
                .unwrap()
                .contains("лишний")
        );
    }

    #[test]
    fn input_plan() {
        let all = [true; 6];
        assert_eq!(
            plan_input_tracks(&all, &[1, 2], &[3]),
            [None, None, Some(false), None, None, None]
        );
        let none = [false; 6];
        assert_eq!(
            plan_input_tracks(&none, &[1, 3], &[2]),
            [Some(true), None, Some(true), None, None, None]
        );
        let ok = [true, true, false, false, false, false];
        assert_eq!(plan_input_tracks(&ok, &[1, 2], &[3]), [None; 6]);
    }

    #[test]
    fn main_plan() {
        let ok = plan_main_profile(Some("Advanced"), Some("7"), Some("mkv"), None, &[1, 2, 3]);
        assert!(ok.is_empty());
        let p = plan_main_profile(Some("Simple"), Some("1"), Some("mp4"), None, &[1, 2, 3]);
        let v: Vec<_> = p.iter().map(|c| (c.name, c.value.as_str())).collect();
        assert_eq!(
            v,
            [
                ("Mode", "Advanced"),
                ("RecTracks", "7"),
                ("RecFormat2", "mkv")
            ]
        );
        let p = plan_main_profile(Some("Advanced"), Some("33"), Some("mkv"), None, &[1, 2, 3]);
        assert_eq!(p[0].value, "39");
    }

    #[test]
    fn audio_plan_only_differences() {
        let all = plan_audio_profile(&|_, _| None, &[1, 2, 3]);
        assert!(
            all.iter()
                .any(|c| c.name == "RecType" && c.value == "FFmpeg")
        );
        assert!(
            all.iter()
                .any(|c| c.name == "FFAudioMixes" && c.value == "7")
        );
        let done = plan_audio_profile(
            &|_, n| {
                Some(
                    match n {
                        "Mode" => "Advanced",
                        "RecType" => "FFmpeg",
                        "FFOutputToFile" => "true",
                        "FFFormat" => "matroska",
                        "FFFormatMimeType" => "audio/x-matroska",
                        "FFExtension" => "mka",
                        "FFVEncoderId" => "0",
                        "FFAEncoderId" => "86018",
                        "FFAEncoder" => "aac",
                        "FFAudioMixes" => "63",
                        _ => "",
                    }
                    .to_owned(),
                )
            },
            &[1, 2, 3],
        );
        assert!(done.is_empty(), "{done:?}");
    }
}
