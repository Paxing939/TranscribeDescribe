//! Дымовые тесты на живой системе: `cargo test -p callscribe-platform -- --ignored --nocapture`.
#![cfg(windows)]

use callscribe_core::platform_api::{Autostart, MicUsageProvider, WindowProvider};
use callscribe_platform::*;

#[test]
#[ignore]
fn smoke_mic_users() {
    for (name, r) in [
        ("wasapi", WasapiMicProvider::new().mic_users()),
        ("registry", RegistryMicProvider::new().mic_users()),
        ("combined", CombinedMicProvider::new().mic_users()),
    ] {
        println!("{name}: {r:#?}");
        assert!(r.is_ok());
    }
    println!(
        "webcam_in_use = {}",
        RegistryMicProvider::new().webcam_in_use()
    );
}

#[test]
#[ignore]
fn smoke_windows_and_processes() {
    let mut p = Win32WindowProvider::new();
    let w = p.windows().unwrap();
    for x in &w {
        println!("{:>6} {:<28} {}", x.pid, x.exe_name, x.title);
    }
    let procs = p.processes().unwrap();
    println!("windows: {}, processes: {}", w.len(), procs.len());
    assert!(!procs.is_empty());
}

#[test]
#[ignore]
fn smoke_misc() {
    println!("computer = {}", computer_name());
    let free = free_space_bytes(std::path::Path::new(r"C:\no\such\dir")).unwrap();
    println!("free = {} GiB", free >> 30);
    assert!(free > 0);
    println!(
        "autostart enabled = {}",
        RegistryAutostart.is_enabled().unwrap()
    );
}
