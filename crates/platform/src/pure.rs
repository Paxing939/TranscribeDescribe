//! Чистые функции без обращения к системе (тестируются на любой ОС).

use callscribe_core::detection::rules::{ProcessInfo, exe_from_registry_key};

/// Привести запись PATH к виду для сравнения: без кавычек и хвостового `\`, в нижнем регистре.
pub fn normalize_path_entry(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// Есть ли каталог в списке PATH (разделитель `;`).
pub fn path_list_contains(list: &str, dir: &str) -> bool {
    let want = normalize_path_entry(dir);
    list.split(';').any(|e| normalize_path_entry(e) == want)
}

/// Добавить каталог в конец PATH. `None` — каталог уже есть.
pub fn path_list_add(list: &str, dir: &str) -> Option<String> {
    if path_list_contains(list, dir) {
        return None;
    }
    let dir = dir.trim_end_matches(['\\', '/']);
    let base = list.trim_end_matches(';');
    Some(if base.is_empty() {
        dir.to_string()
    } else {
        format!("{base};{dir}")
    })
}

/// Убрать каталог из PATH (все вхождения). `None` — его там не было.
pub fn path_list_remove(list: &str, dir: &str) -> Option<String> {
    if !path_list_contains(list, dir) {
        return None;
    }
    let want = normalize_path_entry(dir);
    let kept: Vec<&str> = list
        .split(';')
        .filter(|e| normalize_path_entry(e) != want)
        .collect();
    Some(kept.join(";"))
}

/// Подключ `ConsentStore` → процесс. Для `NonPackaged` имя — путь к exe с `#` вместо `\`,
/// для упакованных приложений — package family name.
pub fn consent_key_to_process(key_name: &str, non_packaged: bool) -> ProcessInfo {
    if non_packaged {
        ProcessInfo {
            pid: 0,
            exe_name: exe_from_registry_key(key_name),
            package_family: None,
        }
    } else {
        ProcessInfo {
            pid: 0,
            exe_name: key_name.to_string(),
            package_family: Some(key_name.to_string()),
        }
    }
}

/// Системные процессы: на них цепочка родителей обрывается (и в цепочку они не входят).
pub fn is_system_process(exe: &str) -> bool {
    const SYS: &[&str] = &[
        "explorer.exe",
        "services.exe",
        "svchost.exe",
        "wininit.exe",
        "winlogon.exe",
        "system",
        "smss.exe",
        "csrss.exe",
        "lsass.exe",
        "userinit.exe",
        "sihost.exe",
        "dwm.exe",
        "fontdrvhost.exe",
        "system idle process",
        "registry",
    ];
    SYS.iter().any(|s| s.eq_ignore_ascii_case(exe))
}

/// Имя файла из полного пути (поддерживает `\` и `/`).
pub fn file_name_of(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_add_and_dedupe() {
        assert_eq!(path_list_add("", r"C:\App\").as_deref(), Some(r"C:\App"));
        assert_eq!(
            path_list_add(r"C:\a;", r"C:\b").as_deref(),
            Some(r"C:\a;C:\b")
        );
        assert_eq!(path_list_add(r"C:\a;c:\APP\", r"C:\app"), None);
        assert_eq!(path_list_add(r#"C:\a;"C:\app""#, r"C:\app"), None);
    }

    #[test]
    fn path_remove() {
        assert_eq!(
            path_list_remove(r"C:\a;C:\App;%X%\b", r"c:\app\").as_deref(),
            Some(r"C:\a;%X%\b")
        );
        assert_eq!(path_list_remove(r"C:\App", r"C:\app").as_deref(), Some(""));
        assert_eq!(path_list_remove(r"C:\a", r"C:\b"), None);
    }

    #[test]
    fn consent_keys() {
        let p = consent_key_to_process("C:#Program Files#Telegram Desktop#Telegram.exe", true);
        assert_eq!(
            (p.pid, p.exe_name.as_str(), p.package_family),
            (0, "Telegram.exe", None)
        );
        let p = consent_key_to_process("MSTeams_8wekyb3d8bbwe", false);
        assert_eq!(p.package_family.as_deref(), Some("MSTeams_8wekyb3d8bbwe"));
    }

    #[test]
    fn system_and_names() {
        assert!(is_system_process("Explorer.EXE"));
        assert!(!is_system_process("chrome.exe"));
        assert_eq!(file_name_of(r"C:\x\y\Zoom.exe"), "Zoom.exe");
    }
}
