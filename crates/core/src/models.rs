//! Model files: names, locations, download with size and SHA-256 checks (F-54).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub const VAD_MODEL: &str = "ggml-silero-v6.2.0.bin";

pub fn whisper_file(model: &str) -> String {
    format!("ggml-{model}.bin")
}

pub fn whisper_url(model: &str) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        whisper_file(model)
    )
}

pub fn vad_url() -> String {
    format!("https://huggingface.co/ggml-org/whisper-vad/resolve/main/{VAD_MODEL}")
}

/// Download `url` to `dest` unless it already exists. Progress: (done, total).
pub async fn ensure(
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    if dest.is_file() && std::fs::metadata(dest)?.len() > 0 {
        return Ok(dest.to_path_buf());
    }
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d)?;
    }
    tracing::info!("скачиваю {url} → {}", dest.display());

    // Hugging Face reports the LFS SHA-256 and size on the redirect response.
    let head_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut expected_sha: Option<String> = None;
    let mut expected_size: Option<u64> = None;
    if let Ok(r) = head_client.head(url).send().await {
        let h = r.headers();
        expected_sha = h
            .get("x-linked-etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches('"').to_ascii_lowercase())
            .filter(|s| s.len() == 64);
        expected_size = h
            .get("x-linked-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());
    }

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("не удалось скачать {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    let total = expected_size.or(resp.content_length());
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        done += chunk.len() as u64;
        progress(done, total);
    }
    file.flush()?;
    drop(file);
    if let Some(t) = total
        && done != t
    {
        let _ = std::fs::remove_file(&part);
        bail!("{url}: скачано {done} байт из {t}");
    }
    let got = format!("{:x}", hasher.finalize());
    if let Some(exp) = expected_sha
        && exp != got
    {
        let _ = std::fs::remove_file(&part);
        bail!("{url}: SHA-256 не совпал");
    }
    std::fs::rename(&part, dest)?;
    Ok(dest.to_path_buf())
}

// ---------------- sherpa-onnx models (F-60, F-61) ----------------
//
// k2-fsa publishes models on GitHub releases as `.tar.bz2` with one top-level folder named like
// the archive. They are unpacked to `models_dir/<name>/`.

const K2FSA_RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

/// GigaAM v3 RNNT with punctuation, Russian (~170 MB).
pub const GIGAAM_DIR: &str = "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16";
/// Parakeet TDT 0.6B v3, int8, 25 European languages (~490 MB).
pub const PARAKEET_DIR: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
/// pyannote segmentation 3.0 for diarization.
pub const PYANNOTE_DIR: &str = "sherpa-onnx-pyannote-segmentation-3-0";
/// 3D-Speaker voice embeddings (Chinese + English, 16 kHz) for diarization.
pub const SPEAKER_EMBEDDING: &str = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";
/// Silero VAD in the ONNX form sherpa-onnx reads (not the GGML one from whisper.cpp).
pub const SHERPA_VAD: &str = "silero_vad.onnx";

pub fn sherpa_asr_url(dir: &str) -> String {
    format!("{K2FSA_RELEASES}/asr-models/{dir}.tar.bz2")
}

pub fn pyannote_url() -> String {
    format!("{K2FSA_RELEASES}/speaker-segmentation-models/{PYANNOTE_DIR}.tar.bz2")
}

pub fn speaker_embedding_url() -> String {
    format!("{K2FSA_RELEASES}/speaker-recongition-models/{SPEAKER_EMBEDDING}")
}

pub fn sherpa_vad_url() -> String {
    format!("{K2FSA_RELEASES}/asr-models/{SHERPA_VAD}")
}

/// Unpack a `.tar.bz2` into `dest` (created). Entries that escape `dest` are rejected by `tar`.
pub fn extract_tar_bz2(archive: &Path, dest: &Path) -> Result<()> {
    let f = std::fs::File::open(archive).with_context(|| format!("{}", archive.display()))?;
    let dec = bzip2::read::BzDecoder::new(std::io::BufReader::new(f));
    std::fs::create_dir_all(dest)?;
    tar::Archive::new(dec)
        .unpack(dest)
        .with_context(|| format!("не удалось распаковать {}", archive.display()))
}

/// Make sure `models_dir/<name>/` exists: download `url` (`.tar.bz2`) and unpack it.
/// The archive is removed after unpacking. Returns the model folder.
pub async fn ensure_archive(
    url: &str,
    name: &str,
    models_dir: &Path,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let target = models_dir.join(name);
    if target.is_dir() {
        return Ok(target);
    }
    let archive = models_dir.join(format!("{name}.tar.bz2"));
    ensure(url, &archive, progress).await?;
    // Unpack beside the target and rename, so a half-unpacked folder is never taken for a model.
    let tmp = models_dir.join(format!("{name}.unpacking"));
    let _ = std::fs::remove_dir_all(&tmp);
    tracing::info!("распаковываю {}", archive.display());
    if let Err(e) = extract_tar_bz2(&archive, &tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        // A broken download must not stay in the cache.
        let _ = std::fs::remove_file(&archive);
        return Err(e);
    }
    let inner = tmp.join(name);
    let src = if inner.is_dir() { inner } else { tmp.clone() };
    std::fs::rename(&src, &target)?;
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&archive);
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            sherpa_asr_url(PARAKEET_DIR),
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2"
        );
        assert!(pyannote_url().ends_with("sherpa-onnx-pyannote-segmentation-3-0.tar.bz2"));
    }

    #[test]
    fn unpack_and_rename() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        // Build a tiny .tar.bz2 with one top-level folder.
        let archive = dir.path().join("m.tar.bz2");
        {
            let enc = bzip2::write::BzEncoder::new(
                std::fs::File::create(&archive).unwrap(),
                bzip2::Compression::fast(),
            );
            let mut b = tar::Builder::new(enc);
            let data = b"hello";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "m/tokens.txt", &data[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap().flush().unwrap();
        }
        let out = dir.path().join("out");
        extract_tar_bz2(&archive, &out).unwrap();
        assert_eq!(std::fs::read(out.join("m/tokens.txt")).unwrap(), b"hello");
    }
}
