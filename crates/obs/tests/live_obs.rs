//! Интеграционный тест на живом OBS: `cargo test -p callscribe-obs -- --ignored --nocapture`.
//! Нужен запущенный OBS с включённым сервером WebSocket. Порт и пароль можно
//! задать через `OBS_TEST_PORT` / `OBS_TEST_PASSWORD`, иначе берётся конфиг по умолчанию.

use callscribe_core::Config;
use callscribe_obs::ObsClient;

#[tokio::test]
#[ignore = "нужен запущенный OBS"]
async fn connect_and_check_settings() {
    let mut cfg = Config::default().obs;
    if let Ok(p) = std::env::var("OBS_TEST_PORT") {
        cfg.port = p.parse().unwrap();
    }
    if let Ok(p) = std::env::var("OBS_TEST_PASSWORD") {
        cfg.password = p;
    }
    let client = ObsClient::connect(&cfg, None)
        .await
        .expect("подключение к OBS");
    let status = client.record_status().await.unwrap();
    println!("record_status: {status:?}");
    println!("inputs: {:?}", client.special_inputs().await);
    for line in client.check_settings(&cfg).await {
        println!("check: {line}");
    }
    let mut rx = client.subscribe();
    let ev = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    println!("event within 3s: {ev:?}");
}

/// Негативный сценарий: профиля `audio_profile` нет — старт даёт ошибку и ничего не меняет.
#[tokio::test]
#[ignore = "нужен запущенный OBS"]
async fn audio_mode_without_profile_fails_cleanly() {
    use callscribe_core::config::RecordMode;
    let mut cfg = Config::default().obs;
    cfg.audio_profile = "нет-такого-профиля".into();
    if let Ok(p) = std::env::var("OBS_TEST_PASSWORD") {
        cfg.password = p;
    }
    let client = ObsClient::connect(&cfg, None)
        .await
        .expect("подключение к OBS");
    let dir = std::env::temp_dir().join("callscribe-obs-live");
    let before = client.record_directory().await.unwrap();
    let err = client
        .start_recording(RecordMode::Audio, 1, &dir)
        .await
        .unwrap_err();
    println!("ожидаемая ошибка: {err:#}");
    assert_eq!(client.record_directory().await.unwrap(), before);
    assert!(!client.record_status().await.unwrap().active);
}

/// Цикл записи ~3 с во временную папку: старт, пауза, продолжение, стоп, возврат
/// настроек. Режим «экран» со сценой `OBS_TEST_SCENE` (по умолчанию «Scene»).
#[tokio::test]
#[ignore = "нужен запущенный OBS; пишет короткий файл во временную папку"]
async fn record_cycle_screen_mode() {
    use callscribe_core::config::RecordMode;
    use callscribe_obs::ObsEvent;
    use std::time::Duration;
    let mut cfg = Config::default().obs;
    cfg.screen_scenes = vec![std::env::var("OBS_TEST_SCENE").unwrap_or_else(|_| "Scene".into())];
    if let Ok(p) = std::env::var("OBS_TEST_PASSWORD") {
        cfg.password = p;
    }
    let client = ObsClient::connect(&cfg, None)
        .await
        .expect("подключение к OBS");
    let dir = std::env::temp_dir().join("callscribe-obs-live");
    std::fs::create_dir_all(&dir).unwrap();
    let before = client.record_directory().await.unwrap();
    let mut rx = client.subscribe();

    let st = client
        .start_recording(RecordMode::Screen, 1, &dir)
        .await
        .unwrap();
    println!("start: {st:?}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!(
        "running status: {:?}",
        client.record_status().await.unwrap()
    );
    client.pause().await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let s = client.record_status().await.unwrap();
    println!("paused status: {s:?}");
    assert!(s.active);
    // Hybrid MP4 не поддерживает паузу: тогда paused остаётся false.
    let pause_supported = s.paused;
    println!("pause supported: {pause_supported}");
    if pause_supported {
        client.resume().await.unwrap();
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!(
        "resumed status: {:?}",
        client.record_status().await.unwrap()
    );
    let path = client.stop_recording().await.unwrap();
    println!("file: {}", path.display());
    client.restore(&st).await.unwrap();
    assert_eq!(
        client.record_directory().await.unwrap(),
        before,
        "папка записи не вернулась"
    );

    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if !matches!(ev, ObsEvent::Levels { .. }) {
            seen.push(ev);
        }
    }
    println!("events: {seen:?}");
    assert!(seen.iter().any(|e| matches!(e, ObsEvent::RecordStarted)));
    if pause_supported {
        assert!(seen.iter().any(|e| matches!(e, ObsEvent::RecordPaused)));
        assert!(seen.iter().any(|e| matches!(e, ObsEvent::RecordResumed)));
    }
    assert!(
        seen.iter()
            .any(|e| matches!(e, ObsEvent::RecordStopped { .. }))
    );
    assert!(path.starts_with(&dir) || path.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Запись ~3 с в режиме «только звук» (профиль `audio_profile` должен существовать,
/// см. `callscribe obs-setup`). Путь файла печатается — проверить ffprobe.
#[tokio::test]
#[ignore = "нужен запущенный OBS с настроенным профилем; пишет короткий файл"]
async fn record_audio_mode() {
    use callscribe_core::config::RecordMode;
    use std::time::Duration;
    let mut cfg = Config::default().obs;
    if let Ok(p) = std::env::var("OBS_TEST_PASSWORD") {
        cfg.password = p;
    }
    let client = ObsClient::connect(&cfg, None)
        .await
        .expect("подключение к OBS");
    let dir = std::env::temp_dir().join("callscribe-obs-live-audio");
    std::fs::create_dir_all(&dir).unwrap();
    let st = client
        .start_recording(RecordMode::Audio, 1, &dir)
        .await
        .unwrap();
    println!("start: {st:?}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let path = client.stop_recording().await.unwrap();
    println!("FILE={}", path.display());
    client.restore(&st).await.unwrap();
    println!("OK");
}
