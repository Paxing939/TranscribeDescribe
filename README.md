# CallScribe

Фоновое приложение для Windows: замечает звонок, пишет его через OBS, после звонка локально расшифровывает (whisper.cpp на GPU), делает саммари через LLM (DeepSeek / Ollama / Anthropic) и раскладывает файлы по папкам. ТЗ — [callscribe-spec.md](callscribe-spec.md), решения по реализации — [docs/DECISIONS.md](docs/DECISIONS.md).

## Состав

- `callscribe-tray.exe` — трей: детект звонков, оверлеи, управление OBS, очередь обработки.
- `callscribe.exe` — CLI и worker-процесс обработки.

| Команда | Что делает |
|---|---|
| `callscribe` | запустить трей |
| `callscribe process <файл или папка> [--app X] [--title Y] [--no-cloud] [--keep-source]` | расшифровка → саммари → название для готовой записи |
| `callscribe resummarize <папка звонка>` | пересоздать саммари |
| `callscribe doctor [--model] [--json]` | проверить OBS, ffmpeg, GPU, модели, LLM, место на диске |
| `callscribe reindex` | пересобрать индекс звонков из `meta.json` |
| `callscribe secret set <имя>` | сохранить ключ (например, `deepseek`) в хранилище Windows |
| `callscribe paths` | где лежат конфиг, данные, логи, модели |
| `callscribe obs-setup [--check]` | проверить и привести OBS к нужному виду: дорожки, сцены «Call – Экран N», профиль «только звук» (F-27) |
| `callscribe export <папка>` / `callscribe import` | отправить звонок на машину-обработчик / забрать результаты (M7) |
| `callscribe worker --inbox <папка> [--once]` | машина-обработчик пакетов (M7) |

## Первый запуск

1. В OBS включить сервер WebSocket (Инструменты → Настройки сервера WebSocket), затем `callscribe obs-setup` — он сам настроит дорожки, сцены и профиль «только звук» (ТЗ §14).
2. `callscribe paths` — открыть `config.toml` (создаётся с комментариями при первом запуске).
3. DeepSeek: `callscribe secret set deepseek`, затем в `[llm]` поменять значения по примеру из комментария в конфиге.
4. `callscribe doctor` — всё ли в порядке.
5. Проверка на готовом файле: `callscribe process "D:\Записи\2026-10-15 10-00-00.mkv"`. При первой обработке скачается модель `large-v3-turbo` (~1,6 ГБ).

Результат — папка `~/Videos/Calls/<дата>_<время>_<приложение>_<название>/` с видео (или звуком), `.srt`, `transcript.md`, `transcript.json`, `summary.md`, `meta.json`.

## Сборка

Сейчас сборка идёт тулчейном MSYS2 UCRT64 (см. D-001):

```powershell
. .\scripts\env.ps1
cargo test --workspace --exclude callscribe-tray
cargo build --release -p callscribe-cli
.\scripts\build-installer.ps1      # NSIS-установщик трея + CLI + ffmpeg
```

Нужны: MSYS2 с пакетами `mingw-w64-ucrt-x86_64-{rust,gcc,clang,cmake,ninja,vulkan-headers,vulkan-loader,shaderc}`, Node.js, `cargo install tauri-cli`, ffmpeg в PATH.

Структура — ТЗ §4.2: `crates/core` (логика без платформы), `crates/media` (ffmpeg), `crates/stt-whisper` (whisper.cpp), `crates/obs` (obs-websocket), `crates/platform` (WASAPI, окна, реестр), `apps/cli`, `apps/tray` (Tauri 2 + Svelte).
