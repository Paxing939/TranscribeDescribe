//! ffmpeg / ffprobe wrappers (F-52, F-53) and WAV reading.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    /// Index among audio streams (`0:a:N`).
    pub audio_index: u32,
    pub codec: String,
    pub channels: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProbeInfo {
    pub duration_sec: f64,
    pub has_video: bool,
    pub audio: Vec<AudioStream>,
    pub format: String,
}

/// Hide the console window of child processes started from the GUI app (§11, п. 25).
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(&exe))
            .find(|p| p.is_file())
    })
}

impl Ffmpeg {
    /// `dir = "auto"`: next to the current exe, then PATH (§6 `pipeline.ffmpeg_dir`).
    pub fn locate(dir: &str) -> Result<Ffmpeg> {
        let exe = |n: &str| {
            if cfg!(windows) {
                format!("{n}.exe")
            } else {
                n.to_string()
            }
        };
        let find = |name: &str| -> Option<PathBuf> {
            if dir != "auto" && !dir.is_empty() {
                let p = Path::new(dir).join(exe(name));
                return p.is_file().then_some(p);
            }
            let beside = std::env::current_exe().ok()?.parent()?.join(exe(name));
            if beside.is_file() {
                return Some(beside);
            }
            which(name)
        };
        let ffmpeg = find("ffmpeg")
            .context("ffmpeg не найден (рядом с exe, в PATH или pipeline.ffmpeg_dir)")?;
        let ffprobe = find("ffprobe")
            .context("ffprobe не найден (рядом с exe, в PATH или pipeline.ffmpeg_dir)")?;
        Ok(Ffmpeg { ffmpeg, ffprobe })
    }

    pub fn version(&self) -> Result<String> {
        let out = no_window(&mut Command::new(&self.ffmpeg))
            .arg("-version")
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string())
    }

    pub fn probe(&self, path: &Path) -> Result<ProbeInfo> {
        let out = no_window(&mut Command::new(&self.ffprobe))
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
            ])
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .context("не удалось запустить ffprobe")?;
        if !out.status.success() {
            bail!(
                "ffprobe {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_probe(&String::from_utf8_lossy(&out.stdout))
    }

    fn run(&self, args: &[&std::ffi::OsStr]) -> Result<()> {
        let out = no_window(&mut Command::new(&self.ffmpeg))
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("не удалось запустить ffmpeg")?;
        if !out.status.success() {
            bail!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    /// Remux with all streams, no re-encoding (F-52, §11 п. 7). Checks the duration afterwards.
    pub fn remux(&self, input: &Path, output: &Path) -> Result<()> {
        let tmp = output.with_extension(format!(
            "partial.{}",
            output.extension().and_then(|e| e.to_str()).unwrap_or("mp4")
        ));
        self.run(&[
            "-i".as_ref(),
            input.as_os_str(),
            "-map".as_ref(),
            "0".as_ref(),
            "-c".as_ref(),
            "copy".as_ref(),
            tmp.as_os_str(),
        ])?;
        let a = self.probe(input)?.duration_sec;
        let b = self.probe(&tmp)?.duration_sec;
        if (a - b).abs() > 2.0_f64.max(a * 0.01) {
            let _ = std::fs::remove_file(&tmp);
            bail!("ремукс: длительность не совпала ({a:.1} с → {b:.1} с)");
        }
        std::fs::rename(&tmp, output)?;
        Ok(())
    }

    /// One audio stream → 16 kHz mono WAV (F-53).
    pub fn extract_track(&self, input: &Path, audio_index: u32, output: &Path) -> Result<()> {
        let map = format!("0:a:{audio_index}");
        self.run(&[
            "-i".as_ref(),
            input.as_os_str(),
            "-map".as_ref(),
            map.as_ref(),
            "-ac".as_ref(),
            "1".as_ref(),
            "-ar".as_ref(),
            "16000".as_ref(),
            "-c:a".as_ref(),
            "pcm_s16le".as_ref(),
            output.as_os_str(),
        ])
    }

    /// Mix all audio streams into one 16 kHz mono WAV (fallback for odd files).
    pub fn extract_mix(&self, input: &Path, output: &Path) -> Result<()> {
        self.run(&[
            "-i".as_ref(),
            input.as_os_str(),
            "-ac".as_ref(),
            "1".as_ref(),
            "-ar".as_ref(),
            "16000".as_ref(),
            "-c:a".as_ref(),
            "pcm_s16le".as_ref(),
            output.as_os_str(),
        ])
    }

    /// Run ffmpeg with arbitrary arguments (tests, fixtures).
    pub fn raw(&self, args: &[&str]) -> Result<()> {
        let a: Vec<&std::ffi::OsStr> = args.iter().map(|s| s.as_ref()).collect();
        self.run(&a)
    }
}

#[derive(Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<StreamJson>,
    format: Option<FormatJson>,
}
#[derive(Deserialize)]
struct StreamJson {
    codec_type: Option<String>,
    codec_name: Option<String>,
    channels: Option<u32>,
    disposition: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct FormatJson {
    duration: Option<String>,
    format_name: Option<String>,
}

pub fn parse_probe(json: &str) -> Result<ProbeInfo> {
    let p: ProbeJson = serde_json::from_str(json).context("ffprobe: не JSON")?;
    let mut audio = Vec::new();
    let mut has_video = false;
    for s in &p.streams {
        match s.codec_type.as_deref() {
            Some("audio") => audio.push(AudioStream {
                audio_index: audio.len() as u32,
                codec: s.codec_name.clone().unwrap_or_default(),
                channels: s.channels.unwrap_or(0),
            }),
            Some("video") => {
                let cover = s
                    .disposition
                    .as_ref()
                    .and_then(|d| d.get("attached_pic"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    == 1;
                has_video |= !cover;
            }
            _ => {}
        }
    }
    let f = p.format.as_ref();
    Ok(ProbeInfo {
        duration_sec: f
            .and_then(|f| f.duration.as_deref())
            .and_then(|d| d.parse().ok())
            .unwrap_or(0.0),
        has_video,
        audio,
        format: f.and_then(|f| f.format_name.clone()).unwrap_or_default(),
    })
}

/// Can the file be opened exclusively (nobody else has it open for writing)?
pub fn can_open_exclusive(path: &Path) -> bool {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(path).is_ok()
}

/// Wait until the file stops growing and can be opened exclusively (F-52, §11 п. 6).
pub fn wait_until_released(path: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    let mut last = None;
    let mut stable = 0;
    loop {
        let size = std::fs::metadata(path)
            .with_context(|| format!("нет файла {}", path.display()))?
            .len();
        if Some(size) == last && can_open_exclusive(path) {
            stable += 1;
            if stable >= 2 {
                return Ok(());
            }
        } else {
            stable = 0;
        }
        last = Some(size);
        if start.elapsed() > timeout {
            bail!("файл {} так и не освободился", path.display());
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

/// Read a 16-bit or float WAV as mono f32 (averaging channels).
pub fn read_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path)
        .with_context(|| format!("не удалось открыть {}", path.display()))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono = if ch == 1 {
        samples
    } else {
        samples
            .chunks(ch)
            .map(|c| c.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    Ok((mono, spec.sample_rate))
}

/// Peak level of a PCM buffer in dBFS (empty-track check).
pub fn peak_db(pcm: &[f32]) -> f64 {
    let peak = pcm.iter().fold(0f32, |m, &s| m.max(s.abs())) as f64;
    if peak <= 1e-9 {
        -120.0
    } else {
        20.0 * peak.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_parsing() {
        let j = r#"{"streams":[
            {"codec_type":"video","codec_name":"h264"},
            {"codec_type":"audio","codec_name":"aac","channels":2},
            {"codec_type":"audio","codec_name":"aac","channels":1},
            {"codec_type":"audio","codec_name":"aac","channels":2}],
          "format":{"duration":"2712.5","format_name":"matroska,webm"}}"#;
        let p = parse_probe(j).unwrap();
        assert!(p.has_video);
        assert_eq!(p.audio.len(), 3);
        assert_eq!(p.audio[2].audio_index, 2);
        assert_eq!(p.duration_sec, 2712.5);
    }

    #[test]
    fn wav_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..10 {
            w.write_sample(16384i16).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let (pcm, sr) = read_wav_mono_f32(&p).unwrap();
        assert_eq!((pcm.len(), sr), (10, 16000));
        assert!((pcm[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    #[ignore = "needs ffmpeg in PATH"]
    fn real_ffmpeg_three_tracks() {
        let f = Ffmpeg::locate("auto").unwrap();
        let d = tempfile::tempdir().unwrap();
        let mkv = d.path().join("t.mkv");
        f.raw(&[
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=f=880:d=2",
            "-map",
            "0:a",
            "-map",
            "0:a",
            "-map",
            "1:a",
            "-c:a",
            "aac",
            mkv.to_str().unwrap(),
        ])
        .unwrap();
        let p = f.probe(&mkv).unwrap();
        assert_eq!(p.audio.len(), 3);
        let wav = d.path().join("me.wav");
        f.extract_track(&mkv, 1, &wav).unwrap();
        let (pcm, sr) = read_wav_mono_f32(&wav).unwrap();
        assert_eq!(sr, 16000);
        assert!(pcm.len() > 30000);
        f.remux(&mkv, &d.path().join("t.mp4")).unwrap();
    }
}
