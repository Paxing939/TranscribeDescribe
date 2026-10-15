//! `callscribe obs-setup [--check]` — проверка и автонастройка OBS (F-27).

use std::time::Duration;

use anyhow::{Context, Result};
use callscribe_core::Config;
use callscribe_core::llm::SecretResolver;
use callscribe_obs::ObsClient;

use crate::secrets::Keyring;

fn print_problems(problems: &[String]) {
    if problems.is_empty() {
        println!("Настройки OBS в порядке.");
    } else {
        println!("Найдены расхождения ({}):", problems.len());
        for p in problems {
            println!("  - {p}");
        }
    }
}

pub fn run(cfg: &Config, check_only: bool) -> Result<()> {
    let secret = match cfg.obs.password.strip_prefix("keyring:") {
        Some(name) => Some(Keyring.get(name)?),
        None => None,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = tokio::time::timeout(
            Duration::from_secs(10),
            ObsClient::connect(&cfg.obs, secret),
        )
        .await
        .context("OBS не ответил за 10 с")??;

        let problems = client.check_settings(&cfg.obs).await;
        print_problems(&problems);
        if check_only || problems.is_empty() {
            return Ok(());
        }

        println!("\nИсправляю…");
        let result = client.fix_settings(&cfg.obs).await;
        match &result {
            Ok(changes) => {
                for c in changes {
                    println!("  + {c}");
                }
            }
            Err(e) => eprintln!("{e:#}"),
        }
        println!("\nПовторная проверка:");
        print_problems(&client.check_settings(&cfg.obs).await);
        result.map(|_| ())
    })
}
