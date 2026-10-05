//! Resumable, verified downloads.
//!
//! Data goes to `<dest>.part`; an interrupted download continues where it
//! stopped (HTTP `Range`). The SHA-256 is computed while downloading and
//! checked at the end – a mismatching file is discarded, never used.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ancilo_core::{Error, EventBus, Result};
use futures::StreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub url: String,
    pub dest: PathBuf,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    /// Bearer token for this URL only (Hugging Face `HF_TOKEN`).
    pub bearer: Option<String>,
    /// What it is, for the user's log of what left this computer.
    pub note: ancilo_net::Note,
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub max_attempts: u32,
    pub base_backoff: Duration,
    pub progress_interval: Duration,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            base_backoff: Duration::from_secs(1),
            progress_interval: Duration::from_millis(500),
        }
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

async fn hash_existing(path: &Path, hasher: &mut Sha256) -> Result<u64> {
    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}

enum Attempt {
    Done,
    Retry(String),
}

struct Progress<'a> {
    bus: &'a EventBus,
    subject: &'a str,
    total: Option<u64>,
    last_emit: Instant,
    last_bytes: u64,
    interval: Duration,
}

impl Progress<'_> {
    fn update(&mut self, bytes: u64, force: bool) {
        let elapsed = self.last_emit.elapsed();
        if !force && elapsed < self.interval {
            return;
        }
        let rate = if elapsed.as_secs_f64() > 0.0 {
            (bytes.saturating_sub(self.last_bytes)) as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };
        let percent = self
            .total
            .filter(|t| *t > 0)
            .map(|t| (bytes as f64 / t as f64 * 100.0).min(100.0));
        self.bus.emit(
            "download.progress",
            Some(self.subject),
            json!({"bytes": bytes, "total": self.total, "percent": percent, "bytes_per_sec": rate.round()}),
        );
        self.last_emit = Instant::now();
        self.last_bytes = bytes;
    }
}

async fn attempt(
    http: &ancilo_net::Net,
    spec: &DownloadSpec,
    part: &Path,
    hasher: &mut Sha256,
    have: &mut u64,
    progress: &mut Progress<'_>,
    cancel: &CancellationToken,
) -> Result<Attempt> {
    let mut req = http.get(&spec.url);
    if let Some(token) = &spec.bearer {
        req = req.bearer_auth(token);
    }
    if *have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = match http.send(req, spec.note.clone()).await {
        Ok(r) => r,
        Err(e) => return Ok(Attempt::Retry(format!("connection failed: {e}"))),
    };
    let status = resp.status();
    if status.as_u16() == 404 {
        return Err(Error::not_found(format!(
            "download not found: {}",
            spec.url
        )));
    }
    if status.as_u16() == 416 && spec.size == Some(*have) {
        return Ok(Attempt::Done);
    }
    if !status.is_success() {
        return Ok(Attempt::Retry(format!("HTTP {status}")));
    }
    let mut file = if *have > 0 && status.as_u16() == 206 {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await?
    } else {
        // Server ignored the range (or fresh start): begin from scratch.
        *have = 0;
        *hasher = Sha256::new();
        tokio::fs::File::create(part).await?
    };
    let mut stream = resp.bytes_stream();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                file.flush().await?;
                return Err(Error::Conflict(ancilo_core::msg("download.cancelled", &[])));
            }
            chunk = stream.next() => match chunk {
                None => break,
                Some(Err(e)) => {
                    file.flush().await?;
                    return Ok(Attempt::Retry(format!("transfer interrupted: {e}")));
                }
                Some(Ok(bytes)) => {
                    file.write_all(&bytes).await?;
                    hasher.update(&bytes);
                    *have += bytes.len() as u64;
                    progress.update(*have, false);
                }
            }
        }
    }
    file.flush().await?;
    if let Some(size) = spec.size
        && *have < size
    {
        return Ok(Attempt::Retry(format!(
            "incomplete: {have} of {size} bytes"
        )));
    }
    Ok(Attempt::Done)
}

/// Downloads `spec.url` to `spec.dest`, resuming a previous `.part` file.
///
/// Emits `download.started`, `download.progress`, `download.retrying`,
/// `download.completed` and `download.failed` with `subject`.
pub async fn download(
    http: &ancilo_net::Net,
    spec: &DownloadSpec,
    bus: &EventBus,
    subject: &str,
    options: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<()> {
    if let Some(parent) = spec.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_path(&spec.dest);
    let mut hasher = Sha256::new();
    let mut have = hash_existing(&part, &mut hasher).await?;
    bus.emit(
        "download.started",
        Some(subject),
        json!({"url": spec.url, "dest": spec.dest, "total": spec.size, "resumed_at": have}),
    );
    let mut progress = Progress {
        bus,
        subject,
        total: spec.size,
        last_emit: Instant::now(),
        last_bytes: have,
        interval: options.progress_interval,
    };
    let mut attempts = 0;
    loop {
        attempts += 1;
        match attempt(
            http,
            spec,
            &part,
            &mut hasher,
            &mut have,
            &mut progress,
            cancel,
        )
        .await
        {
            Ok(Attempt::Done) => break,
            Ok(Attempt::Retry(reason)) if attempts < options.max_attempts => {
                let wait = options.base_backoff * 2u32.pow(attempts - 1);
                bus.emit(
                    "download.retrying",
                    Some(subject),
                    json!({"reason": reason, "attempt": attempts, "bytes": have, "wait_ms": wait.as_millis() as u64}),
                );
                tokio::select! {
                    _ = cancel.cancelled() => return Err(Error::Conflict(ancilo_core::msg("download.cancelled", &[]))),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
            Ok(Attempt::Retry(reason)) => {
                let err = Error::unavailable(ancilo_core::msg(
                    "download.failed",
                    &[("attempts", &attempts), ("reason", &reason)],
                ));
                bus.emit(
                    "download.failed",
                    Some(subject),
                    json!({"reason": err.message()}),
                );
                return Err(err);
            }
            Err(e) => {
                bus.emit(
                    "download.failed",
                    Some(subject),
                    json!({"reason": e.message()}),
                );
                return Err(e);
            }
        }
    }
    progress.update(have, true);
    let actual = hex::encode(hasher.finalize());
    if let Some(expected) = &spec.sha256
        && !expected.eq_ignore_ascii_case(&actual)
    {
        tokio::fs::remove_file(&part).await.ok();
        let err = Error::Conflict(ancilo_core::msg(
            "download.corrupt",
            &[("path", &spec.dest.display())],
        ));
        bus.emit(
            "download.failed",
            Some(subject),
            json!({"reason": err.message()}),
        );
        return Err(err);
    }
    tokio::fs::rename(&part, &spec.dest).await?;
    bus.emit(
        "download.completed",
        Some(subject),
        json!({"dest": spec.dest, "bytes": have, "sha256": actual}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_testkit::{FakeFile, FakeHf, FakeRepo};

    fn fast() -> DownloadOptions {
        DownloadOptions {
            max_attempts: 3,
            base_backoff: Duration::from_millis(10),
            progress_interval: Duration::from_millis(1),
        }
    }

    fn note() -> ancilo_net::Note {
        ancilo_net::Note::new(
            ancilo_net::Purpose::ModelDownload,
            "o/r/m.gguf",
            ancilo_net::By::You,
        )
    }

    async fn setup(file: FakeFile) -> (FakeHf, DownloadSpec, tempfile::TempDir) {
        let size = file.content.len() as u64;
        let sha = file.sha256();
        let hf = FakeHf::start(vec![FakeRepo::new("o/r", vec![file])]).await;
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec {
            url: format!("{}/o/r/resolve/main/m.gguf", hf.url()),
            dest: dir.path().join("sub/m.gguf"),
            size: Some(size),
            sha256: Some(sha),
            bearer: None,
            note: note(),
        };
        (hf, spec, dir)
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let (_hf, spec, _dir) = setup(FakeFile::gguf("m.gguf", "llama", 4096, 300_000)).await;
        let bus = EventBus::in_memory();
        let mut rx = bus.subscribe();
        download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &bus,
            "m",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::metadata(&spec.dest).unwrap().len(), 300_000);
        let mut kinds = Vec::new();
        while let Ok(e) = rx.try_recv() {
            kinds.push(e.kind);
        }
        assert_eq!(kinds.first().map(String::as_str), Some("download.started"));
        assert!(kinds.iter().any(|k| k == "download.progress"));
        assert_eq!(kinds.last().map(String::as_str), Some("download.completed"));
    }

    // covers: M1-AC-04
    #[tokio::test]
    async fn resumes_after_interruption() {
        let (hf, spec, _dir) =
            setup(FakeFile::gguf("m.gguf", "llama", 4096, 500_000).failing_once_after(200_000))
                .await;
        let bus = EventBus::in_memory();
        download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &bus,
            "m",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::metadata(&spec.dest).unwrap().len(), 500_000);
        let ranged: Vec<_> = hf
            .requests()
            .into_iter()
            .filter(|r| r.range.is_some())
            .collect();
        assert_eq!(ranged.len(), 1, "second attempt must resume with Range");
        assert!(ranged[0].range.as_ref().unwrap().starts_with("bytes="));
        assert_ne!(ranged[0].range.as_deref(), Some("bytes=0-"));
    }

    // covers: M1-AC-04
    #[tokio::test]
    async fn resumes_a_part_file_from_an_earlier_run() {
        let file = FakeFile::gguf("m.gguf", "llama", 4096, 200_000);
        let head = file.content[..50_000].to_vec();
        let (hf, spec, _dir) = setup(file).await;
        std::fs::create_dir_all(spec.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&spec.dest), head).unwrap();
        download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &EventBus::in_memory(),
            "m",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(hf.requests()[0].range.as_deref(), Some("bytes=50000-"));
    }

    // covers: M1-AC-04
    #[tokio::test]
    async fn rejects_and_discards_corrupt_files() {
        let (_hf, spec, _dir) =
            setup(FakeFile::gguf("m.gguf", "llama", 4096, 100_000).corrupt()).await;
        let err = download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &EventBus::in_memory(),
            "m",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.message().contains("checksum"));
        assert!(!spec.dest.exists());
        assert!(!part_path(&spec.dest).exists());
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_and_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec {
            url: "http://127.0.0.1:9/x".into(),
            dest: dir.path().join("x"),
            size: None,
            sha256: None,
            bearer: None,
            note: note(),
        };
        let err = download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &EventBus::in_memory(),
            "x",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "unavailable");
        let hf = FakeHf::start(vec![FakeRepo::new("o/r", vec![])]).await;
        let spec = DownloadSpec {
            url: format!("{}/o/r/resolve/main/nope.gguf", hf.url()),
            ..spec
        };
        let err = download(
            &ancilo_net::Net::new(reqwest::Client::new(), None),
            &spec,
            &EventBus::in_memory(),
            "x",
            &fast(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
