//! `callscribe.exe` — CLI and worker process (F-130, §4.1).

mod doctor;
mod obs_setup;
mod remote;
mod secrets;
mod worker;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use callscribe_core::pipeline::ingest::{self, NewCall};
use callscribe_core::pipeline::meta::{AppInfo, CallStatus, Meta};
use callscribe_core::pipeline::naming::slug;
use callscribe_core::storage::{Db, JobKind};
use callscribe_core::{AppPaths, Config};
use clap::{Parser, Subcommand};

use crate::worker::{JobOptions, Worker};

#[derive(Parser)]
#[command(
    name = "callscribe",
    version,
    about = "CallScribe: запись, расшифровка и саммари звонков"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Прогнать пайплайн для готового файла или папки звонка.
    Process {
        /// Файл записи (mkv/mp4/mka/m4a/wav…) или папка звонка с meta.json.
        path: PathBuf,
        /// Приложение звонка (для имени папки и саммари).
        #[arg(long)]
        app: Option<String>,
        /// Название звонка; без него название предложит LLM.
        #[arg(long)]
        title: Option<String>,
        /// Не отправлять транскрипт в облачную LLM (F-71).
        #[arg(long)]
        no_cloud: bool,
        /// Не переносить исходный файл, а скопировать.
        #[arg(long)]
        keep_source: bool,
        /// Только поставить в очередь, не обрабатывать сейчас.
        #[arg(long)]
        enqueue_only: bool,
    },
    /// Пересоздать саммари звонка.
    Resummarize { call_dir: PathBuf },
    /// Проверить OBS, ffmpeg, GPU, модель, LLM, место на диске.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Не закрывать консоль до нажатия Enter (запуск из трея).
        #[arg(long)]
        wait: bool,
        /// Пробная загрузка модели распознавания (долго).
        #[arg(long)]
        model: bool,
    },
    /// Пересобрать БД из meta.json в calls_dir.
    Reindex,
    /// Выгрузить записанный звонок пакетом на обработку в pipeline.remote.outbox_dir (F-141).
    Export { call_dir: PathBuf },
    /// Машина-обработчик без UI: обрабатывать пакеты из папки (F-143).
    Worker {
        /// Папка пакетов (по умолчанию worker.inbox_dir).
        #[arg(long)]
        inbox: Option<PathBuf>,
        /// Обработать готовые пакеты и выйти.
        #[arg(long)]
        once: bool,
    },
    /// Забрать результаты обработки из outbox_dir в папки звонков (F-144).
    Import,
    /// Служебная: worker-процесс очереди (запускает трей).
    #[command(hide = true)]
    RunQueue,
    /// Служебная: удалить автозапуск и запись в PATH (вызывает деинсталлятор).
    #[command(hide = true)]
    Cleanup,
    /// Секреты в хранилище Windows (ключи API, пароль OBS).
    Secret {
        #[command(subcommand)]
        action: SecretCmd,
    },
    /// Проверить настройки OBS и (без --check) исправить их (F-27).
    ObsSetup {
        /// Только показать расхождения, ничего не менять.
        #[arg(long)]
        check: bool,
    },
    /// Показать пути к конфигу, данным и логам.
    Paths,
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Сохранить секрет: `callscribe secret set deepseek` (значение спросит или возьмёт из --value).
    Set {
        name: String,
        #[arg(long)]
        value: Option<String>,
    },
    Delete {
        name: String,
    },
}

fn init_logging(
    paths: &AppPaths,
    file_prefix: &str,
    console: bool,
) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(file_prefix)
        .filename_suffix("log")
        .max_log_files(7)
        .build(paths.logs_dir())
        .expect("log dir");
    let (nb, guard) = tracing_appender::non_blocking(appender);
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("CALLSCRIBE_LOG").unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("info,whisper_rs=warn,reqwest=warn,hyper=warn")
        })
    };
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(nb)
        .with_ansi(false)
        .with_filter(filter());
    let console_layer = console.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .without_time()
            .with_filter(filter())
    });
    tracing_subscriber::registry()
        .with(file_layer)
        .with(console_layer)
        .init();
    guard
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("ошибка: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::new();
    paths.ensure()?;

    let Some(cmd) = cli.cmd else {
        return start_tray();
    };
    let prefix = match cmd {
        Cmd::RunQueue => "worker",
        _ => "cli",
    };
    let _guard = init_logging(&paths, prefix, !matches!(cmd, Cmd::RunQueue));

    match cmd {
        Cmd::ObsSetup { check } => {
            let cfg = Config::load_default_location(&paths)?;
            obs_setup::run(&cfg, check)
        }
        Cmd::Paths => {
            println!("конфиг:  {}", paths.config_file().display());
            println!("данные:  {}", paths.data_dir().display());
            println!("логи:    {}", paths.logs_dir().display());
            println!("модели:  {}", paths.models_dir().display());
            Ok(())
        }
        Cmd::Secret { action } => match action {
            SecretCmd::Set { name, value } => {
                let v = match value {
                    Some(v) => v,
                    None => {
                        eprint!("значение для «{name}»: ");
                        let mut s = String::new();
                        std::io::stdin().read_line(&mut s)?;
                        s.trim().to_string()
                    }
                };
                if v.is_empty() {
                    bail!("пустое значение");
                }
                secrets::set(&name, &v)?;
                println!("сохранено: keyring:{name}");
                Ok(())
            }
            SecretCmd::Delete { name } => secrets::delete(&name),
        },
        Cmd::Cleanup => {
            let exe_dir = std::env::current_exe()?.parent().unwrap().to_path_buf();
            callscribe_platform::cleanup(&exe_dir)
        }
        Cmd::Doctor { json, wait, model } => {
            let cfg = Config::load_default_location(&paths);
            let r = doctor::run(&paths, cfg, model);
            doctor::print(&r, json);
            if wait {
                eprintln!("\nНажмите Enter, чтобы закрыть окно…");
                let mut s = String::new();
                let _ = std::io::stdin().read_line(&mut s);
            }
            Ok(())
        }
        Cmd::Reindex => {
            let cfg = Config::load_default_location(&paths)?;
            reindex(&paths, cfg)
        }
        Cmd::Export { call_dir } => {
            let cfg = Config::load_default_location(&paths)?;
            let dir = strip_verbatim(
                &call_dir
                    .canonicalize()
                    .with_context(|| format!("нет папки {}", call_dir.display()))?,
            );
            remote::export(&Worker::new(paths, cfg)?, &dir)
        }
        Cmd::Worker { inbox, once } => {
            let cfg = Config::load_default_location(&paths)?;
            let inbox = inbox
                .or_else(|| cfg.worker.inbox_dir.as_deref().map(callscribe_core::config::expand_home))
                .context("укажите --inbox <папка> или worker.inbox_dir в config.toml")?;
            remote::run_worker(&Worker::new(paths, cfg)?, &inbox, once)
        }
        Cmd::Import => {
            let cfg = Config::load_default_location(&paths)?;
            remote::import(&Worker::new(paths, cfg)?)
        }
        Cmd::RunQueue => {
            let cfg = Config::load_default_location(&paths)?;
            Worker::new(paths, cfg)?.run_queue()
        }
        Cmd::Resummarize { call_dir } => {
            let cfg = Config::load_default_location(&paths)?;
            let dir = call_dir
                .canonicalize()
                .with_context(|| format!("нет папки {}", call_dir.display()))?;
            let dir = strip_verbatim(&dir);
            Meta::load(&dir)?;
            let w = Worker::new(paths, cfg)?;
            let auto_name = Meta::load(&dir)?.status != CallStatus::AwaitingName;
            w.db.enqueue(
                &dir,
                JobKind::Summarize,
                &serde_json::to_value(JobOptions {
                    auto_name,
                    keep_source: true,
                })?,
            )?;
            w.db.retry_now()?;
            w.run_queue()?;
            report_last(&w.db)
        }
        Cmd::Process {
            path,
            app,
            title,
            no_cloud,
            keep_source,
            enqueue_only,
        } => {
            let cfg = Config::load_default_location(&paths)?;
            let path = strip_verbatim(
                &path
                    .canonicalize()
                    .with_context(|| format!("нет такого пути: {}", path.display()))?,
            );
            let call_dir = if path.is_dir() {
                let mut m = Meta::load(&path)?;
                if let Some(t) = title {
                    m.title = Some(t);
                    m.title_source = Some("user".into());
                }
                if no_cloud {
                    m.cloud_allowed = false;
                }
                m.save(&path)?;
                path
            } else {
                let ff = callscribe_media::Ffmpeg::locate(&cfg.pipeline.ffmpeg_dir)?;
                let probe = ff.probe(&path)?;
                let app_name = app.unwrap_or_else(|| "Запись".to_string());
                let tag = if app_name == "Запись" {
                    "call".to_string()
                } else {
                    slug(&app_name, false)
                };
                let (dir, _) = ingest::create_call(
                    &cfg,
                    NewCall {
                        started_at: ingest::guess_started(&path, probe.duration_sec),
                        app: AppInfo {
                            rule: app_name,
                            tag,
                            process: None,
                            window_title: None,
                        },
                        trigger: "manual",
                        source_file: &path,
                        user_title: title,
                        cloud_allowed: !no_cloud,
                    },
                )?;
                dir
            };
            let w = Worker::new(paths, cfg)?;
            let id = w.db.enqueue(
                &call_dir,
                JobKind::Process,
                &serde_json::to_value(JobOptions {
                    auto_name: true,
                    keep_source,
                })?,
            )?;
            w.db.retry_now()?;
            println!("в очереди (задание {id}): {}", call_dir.display());
            if enqueue_only {
                return Ok(());
            }
            w.run_queue()?;
            report_last(&w.db)
        }
    }
}

/// `\\?\C:\x` → `C:\x` (canonicalize on Windows), so paths stay readable and compatible.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

fn report_last(db: &Db) -> Result<()> {
    for j in db.recent_jobs(5)?.into_iter().rev() {
        let status = match j.status.as_str() {
            "done" => "готово",
            "failed" => "ошибка",
            "retry" => "ждёт повтора",
            "queued" => "в очереди",
            "running" => "в работе",
            s => s,
        };
        print!("[{}] {status}: {}", j.id, j.call_dir.display());
        if let Some(e) = j.error.filter(|_| j.status != "done") {
            print!(" — {e}");
        }
        println!();
    }
    Ok(())
}

fn reindex(paths: &AppPaths, cfg: Config) -> Result<()> {
    let db = Db::open(&paths.db_file())?;
    db.clear_calls()?;
    let w = Worker::new(paths.clone(), cfg.clone())?;
    let mut n = 0;
    for e in std::fs::read_dir(cfg.calls_dir())?.flatten() {
        let p = e.path();
        if p.join("meta.json").is_file() {
            match w.index(&p) {
                Ok(()) => n += 1,
                Err(err) => eprintln!("{}: {err:#}", p.display()),
            }
        }
    }
    println!("проиндексировано звонков: {n}");
    Ok(())
}

fn start_tray() -> Result<()> {
    let exe = std::env::current_exe()?;
    let tray = exe.with_file_name(if cfg!(windows) {
        "callscribe-tray.exe"
    } else {
        "callscribe-tray"
    });
    if !tray.is_file() {
        bail!(
            "не найден {}; для справки: callscribe --help",
            tray.display()
        );
    }
    std::process::Command::new(&tray)
        .spawn()
        .with_context(|| format!("не удалось запустить {}", tray.display()))?;
    Ok(())
}
