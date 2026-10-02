//! llama.cpp: the pinned `llama-server` build and supervised model processes.
//!
//! Every loaded model runs in its own `llama-server` process on a loopback
//! port, protected by a random API key (so other local programs cannot use it
//! directly). A supervisor restarts crashed processes with backoff.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_core::{Config, Error, EventBus, Paths, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::download::{self, DownloadOptions, DownloadSpec};

/// The llama.cpp build Ancilo uses, pinned by tag and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedBuild {
    pub tag: &'static str,
    pub asset: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

const TAG: &str = "b11270";

/// The pinned build for this platform, if any.
pub fn pinned() -> Option<PinnedBuild> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(PinnedBuild {
            tag: TAG,
            asset: "llama-b11270-bin-macos-arm64.tar.gz",
            sha256: "3d7f0f32a0670c1c24285223037f472822992af691c041400e53cf831f146ea7",
            size: 11_767_616,
        }),
        ("linux", "x86_64") => Some(PinnedBuild {
            tag: TAG,
            asset: "llama-b11270-bin-ubuntu-x64.tar.gz",
            sha256: "b77e6fbfd844bdf7edf54e55d6cfed9c29b50a643ad04392c454828070c166f1",
            size: 17_408_917,
        }),
        _ => None,
    }
}

fn find_server(dir: &Path) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_server(&p) {
                return Some(found);
            }
        } else if p.file_name().is_some_and(|n| n == "llama-server") {
            return Some(p);
        }
    }
    None
}

/// A `llama-server` shipped with Ancilo: in the app bundle next to `ancilo`
/// (`Contents/MacOS/`), or in the archive/Homebrew layout
/// (`bin/ancilo` + `libexec/ancilo/llama-server`).
pub fn bundled_near(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [
        dir.join("llama-server"),
        dir.join("../libexec/ancilo/llama-server"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Marker with the SHA-256 of an installed `llama-server`: it is checked on
/// every start, so a damaged or replaced binary is noticed.
const MARKER: &str = ".llama-server.sha256";

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    std::io::copy(&mut f, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}

/// The installed pinned build in the home, verified against its marker.
fn installed(paths: &Paths, build: &PinnedBuild) -> Result<Option<PathBuf>> {
    let target = paths.bin_dir().join(format!("llama-{}", build.tag));
    let Some(server) = find_server(&target) else {
        return Ok(None);
    };
    let actual = sha256_file(&server)?;
    match std::fs::read_to_string(target.join(MARKER)) {
        Ok(expected) if expected.trim() == actual => Ok(Some(server)),
        Ok(_) => Err(Error::Conflict(format!(
            "the installed llama.cpp was changed or damaged ({}) – reinstall it with `ancilo llama install`",
            server.display()
        ))),
        // Installed before markers existed: it came from the verified archive.
        Err(_) => {
            std::fs::write(target.join(MARKER), &actual)?;
            Ok(Some(server))
        }
    }
}

/// Path of a usable `llama-server`, in this order: the configured override
/// (`llama_server_bin`), the build shipped with Ancilo, the pinned build
/// installed in the home. Downloading the pinned build happens only when
/// allowed (`llama_auto_install`, default: development builds only) –
/// otherwise explicitly with [`install`] (`ancilo llama install`).
pub async fn ensure_binary(
    paths: &Paths,
    config: &Config,
    http: &reqwest::Client,
    bus: &EventBus,
    build: Option<PinnedBuild>,
) -> Result<PathBuf> {
    if let Some(bin) = &config.llama_server_bin {
        if bin.is_file() {
            return Ok(bin.clone());
        }
        return Err(Error::not_found(format!(
            "configured llama-server not found: {}",
            bin.display()
        )));
    }
    // Homebrew links `bin/ancilo` into its prefix: resolve to the real place.
    if let Some(b) = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::canonicalize(e).ok())
        .and_then(|e| bundled_near(&e))
    {
        return Ok(b);
    }
    let build = build.ok_or_else(|| {
        Error::Unavailable(format!(
            "no llama.cpp build for {}/{} yet – set llama_server_bin in config.toml",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    if let Some(server) = installed(paths, &build)? {
        return Ok(server);
    }
    if !config.llama_auto_install.unwrap_or(cfg!(debug_assertions)) {
        return Err(Error::Unavailable(
            "llama.cpp is not installed – this Ancilo package should contain it; install it with `ancilo llama install`".into(),
        ));
    }
    install(paths, config, http, bus, build).await
}

/// Downloads the pinned llama.cpp build (size and SHA-256 checked) into the home.
pub async fn install(
    paths: &Paths,
    config: &Config,
    http: &reqwest::Client,
    bus: &EventBus,
    build: PinnedBuild,
) -> Result<PathBuf> {
    let target = paths.bin_dir().join(format!("llama-{}", build.tag));
    let archive = paths.bin_dir().join(build.asset);
    let spec = DownloadSpec {
        url: format!(
            "{}/ggml-org/llama.cpp/releases/download/{}/{}",
            config.github_endpoint.trim_end_matches('/'),
            build.tag,
            build.asset
        ),
        dest: archive.clone(),
        size: Some(build.size),
        sha256: Some(build.sha256.to_string()),
        bearer: None,
    };
    download::download(
        http,
        &spec,
        bus,
        "llama.cpp",
        &DownloadOptions::default(),
        &CancellationToken::new(),
    )
    .await?;
    let staging = paths
        .bin_dir()
        .join(format!(".extract-{}", uuid::Uuid::new_v4()));
    let (a, s) = (archive.clone(), staging.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&a)?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        tar.set_preserve_permissions(true);
        tar.unpack(&s)
            .map_err(|e| Error::internal(format!("cannot unpack llama.cpp: {e}")))
    })
    .await
    .map_err(Error::internal)??;
    if target.exists() {
        tokio::fs::remove_dir_all(&target).await.ok();
    }
    tokio::fs::rename(&staging, &target).await?;
    tokio::fs::remove_file(&archive).await.ok();
    let server = find_server(&target)
        .ok_or_else(|| Error::internal("llama-server missing in the llama.cpp archive"))?;
    std::fs::write(target.join(MARKER), sha256_file(&server)?)?;
    bus.emit(
        "llama.installed",
        Some("llama.cpp"),
        json!({"tag": build.tag, "path": server}),
    );
    Ok(server)
}

/// How to launch one model.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub binary: PathBuf,
    pub model_path: PathBuf,
    pub alias: String,
    pub ctx_tokens: u64,
    pub gpu_layers: i32,
    pub threads: u32,
    pub embeddings: bool,
    pub log_file: PathBuf,
    pub api_key: String,
    pub api_key_file: PathBuf,
    pub load_timeout: Duration,
    pub env: Vec<(String, String)>,
    /// Restarts allowed within `restart_window` before giving up.
    pub max_restarts: u32,
    pub restart_window: Duration,
    pub restart_backoff: Duration,
    /// Requests worked on at the same time.
    pub parallel: u32,
    /// Priority against the computer's other programs.
    pub priority: crate::resources::Priority,
}

impl LaunchSpec {
    fn args(&self, port: u16) -> Vec<String> {
        let mut a = vec![
            "--model".into(),
            self.model_path.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--alias".into(),
            self.alias.clone(),
            "--ctx-size".into(),
            self.ctx_tokens.to_string(),
            "--n-gpu-layers".into(),
            if self.gpu_layers < 0 {
                "999".into()
            } else {
                self.gpu_layers.to_string()
            },
            "--threads".into(),
            self.threads.to_string(),
            "--api-key-file".into(),
            self.api_key_file.display().to_string(),
            "--no-webui".into(),
            "--parallel".into(),
            self.parallel.max(1).to_string(),
        ];
        if self.embeddings {
            a.push("--embeddings".into());
        } else {
            a.push("--jinja".into());
        }
        a
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    Starting,
    Running,
    Crashed,
    Stopped,
    /// Gave up (repeated crashes or failed start).
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstanceInfo {
    pub state: InstanceState,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub load_ms: Option<u64>,
    pub restarts: u32,
    pub last_error: Option<String>,
}

/// A supervised model process.
pub struct Instance {
    info: Arc<Mutex<InstanceInfo>>,
    stop: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    ready: tokio::sync::watch::Receiver<InstanceState>,
    pub api_key: String,
}

fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

/// Stops model processes left behind by a daemon that died without stopping
/// them (crash, `kill -9`). Only processes started for this home are touched:
/// their command line names this run directory's key file. Returns their pids.
pub fn reap_orphans(run_dir: &Path) -> Vec<u32> {
    let mut stopped = Vec::new();
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return stopped;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "pid") {
            continue;
        }
        let pid: Option<u32> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        std::fs::remove_file(&path).ok();
        let Some(pid) = pid else { continue };
        let key = path.with_extension("key");
        let command = std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        if !command.contains("llama-server") || !command.contains(&key.display().to_string()) {
            continue;
        }
        let target = nix::unistd::Pid::from_raw(pid as i32);
        let _ = nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && nix::sys::signal::kill(target, None).is_ok() {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGKILL);
        stopped.push(pid);
    }
    stopped
}

async fn terminate(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = child.kill().await;
}

enum StartOutcome {
    Ready(Child),
    Exited(String),
    TimedOut(Child),
    Stopped(Child),
}

async fn start_once(
    spec: &LaunchSpec,
    port: u16,
    stop: &CancellationToken,
    http: &reqwest::Client,
) -> Result<StartOutcome> {
    if let Some(dir) = spec.log_file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_file)?;
    let (program, prefix) = crate::resources::priority_command(spec.priority, &spec.binary);
    let mut cmd = Command::new(program);
    cmd.args(prefix)
        .args(spec.args(port))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::internal(format!("cannot start {}: {e}", spec.binary.display())))?;
    // Lets a later daemon stop this process if this one dies without doing so.
    if let Some(pid) = child.id() {
        std::fs::write(spec.api_key_file.with_extension("pid"), pid.to_string()).ok();
    }
    let deadline = Instant::now() + spec.load_timeout;
    let url = format!("http://127.0.0.1:{port}/health");
    loop {
        if stop.is_cancelled() {
            return Ok(StartOutcome::Stopped(child));
        }
        if let Some(status) = child.try_wait()? {
            return Ok(StartOutcome::Exited(format!(
                "llama-server exited during start ({status}): {}",
                log_tail(&spec.log_file)
            )));
        }
        if Instant::now() > deadline {
            return Ok(StartOutcome::TimedOut(child));
        }
        let healthy = http
            .get(&url)
            .bearer_auth(&spec.api_key)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if healthy {
            return Ok(StartOutcome::Ready(child));
        }
        tokio::select! {
            _ = stop.cancelled() => {}
            _ = tokio::time::sleep(Duration::from_millis(150)) => {}
        }
    }
}

impl Instance {
    /// Starts supervising a model process. Returns immediately; use
    /// [`Instance::wait_ready`] to wait for the model to be loaded.
    pub fn spawn(spec: LaunchSpec, bus: EventBus, subject: String) -> Result<Self> {
        std::fs::write(&spec.api_key_file, &spec.api_key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&spec.api_key_file, std::fs::Permissions::from_mode(0o600))?;
        }
        let info = Arc::new(Mutex::new(InstanceInfo {
            state: InstanceState::Starting,
            port: None,
            pid: None,
            started_at: Utc::now(),
            load_ms: None,
            restarts: 0,
            last_error: None,
        }));
        let (tx, rx) = tokio::sync::watch::channel(InstanceState::Starting);
        let stop = CancellationToken::new();
        let api_key = spec.api_key.clone();
        let task = tokio::spawn(supervise(
            spec,
            bus,
            subject,
            info.clone(),
            stop.clone(),
            tx,
        ));
        Ok(Self {
            info,
            stop,
            task: Mutex::new(Some(task)),
            ready: rx,
            api_key,
        })
    }

    pub fn info(&self) -> InstanceInfo {
        self.info.lock().unwrap().clone()
    }

    pub fn base_url(&self) -> Option<String> {
        self.info().port.map(|p| format!("http://127.0.0.1:{p}"))
    }

    /// Waits until the model is running (or failed / stopped).
    pub async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let mut rx = self.ready.clone();
        let wait = async {
            loop {
                match *rx.borrow_and_update() {
                    InstanceState::Running => return Ok(()),
                    InstanceState::Failed | InstanceState::Stopped => {
                        return Err(Error::unavailable(
                            self.info()
                                .last_error
                                .unwrap_or_else(|| "model process did not start".into()),
                        ));
                    }
                    _ => {}
                }
                if rx.changed().await.is_err() {
                    return Err(Error::unavailable("model supervisor ended"));
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| Error::unavailable("timed out waiting for the model to load"))?
    }

    /// Stops the process and waits for the supervisor to finish.
    pub async fn stop(&self) {
        self.stop.cancel();
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

fn set(
    info: &Arc<Mutex<InstanceInfo>>,
    tx: &tokio::sync::watch::Sender<InstanceState>,
    state: InstanceState,
) {
    info.lock().unwrap().state = state;
    let _ = tx.send(state);
}

async fn supervise(
    spec: LaunchSpec,
    bus: EventBus,
    subject: String,
    info: Arc<Mutex<InstanceInfo>>,
    stop: CancellationToken,
    tx: tokio::sync::watch::Sender<InstanceState>,
) {
    let http = reqwest::Client::new();
    let mut crashes: Vec<Instant> = Vec::new();
    let subject = subject.as_str();
    loop {
        let port = match free_port() {
            Ok(p) => p,
            Err(e) => {
                info.lock().unwrap().last_error = Some(e.message());
                set(&info, &tx, InstanceState::Failed);
                return;
            }
        };
        let restarts = info.lock().unwrap().restarts;
        {
            let mut i = info.lock().unwrap();
            i.port = Some(port);
            i.pid = None;
            i.started_at = Utc::now();
        }
        set(&info, &tx, InstanceState::Starting);
        bus.emit(
            "instance.starting",
            Some(subject),
            json!({"port": port, "restarts": restarts}),
        );
        let began = Instant::now();
        let outcome = match start_once(&spec, port, &stop, &http).await {
            Ok(o) => o,
            Err(e) => {
                info.lock().unwrap().last_error = Some(e.message());
                bus.emit(
                    "instance.failed",
                    Some(subject),
                    json!({"reason": e.message()}),
                );
                set(&info, &tx, InstanceState::Failed);
                return;
            }
        };
        let mut child = match outcome {
            StartOutcome::Ready(child) => child,
            StartOutcome::Stopped(mut child) => {
                terminate(&mut child).await;
                bus.emit("instance.stopped", Some(subject), json!({}));
                set(&info, &tx, InstanceState::Stopped);
                return;
            }
            StartOutcome::TimedOut(mut child) => {
                terminate(&mut child).await;
                let msg = format!(
                    "model did not finish loading within {:?}",
                    spec.load_timeout
                );
                info.lock().unwrap().last_error = Some(msg.clone());
                bus.emit("instance.failed", Some(subject), json!({"reason": msg}));
                set(&info, &tx, InstanceState::Failed);
                return;
            }
            StartOutcome::Exited(reason) => {
                // A process that dies while loading is a configuration problem,
                // restarting would not help.
                info.lock().unwrap().last_error = Some(reason.clone());
                bus.emit("instance.failed", Some(subject), json!({"reason": reason}));
                set(&info, &tx, InstanceState::Failed);
                return;
            }
        };
        let load_ms = began.elapsed().as_millis() as u64;
        {
            let mut i = info.lock().unwrap();
            i.pid = child.id();
            i.load_ms = Some(load_ms);
            i.last_error = None;
        }
        if restarts > 0 {
            bus.emit(
                "instance.restarted",
                Some(subject),
                json!({"port": port, "restarts": restarts}),
            );
        }
        bus.emit(
            "instance.ready",
            Some(subject),
            json!({"port": port, "load_ms": load_ms}),
        );
        set(&info, &tx, InstanceState::Running);

        tokio::select! {
            _ = stop.cancelled() => {
                terminate(&mut child).await;
                bus.emit("instance.stopped", Some(subject), json!({}));
                set(&info, &tx, InstanceState::Stopped);
                return;
            }
            status = child.wait() => {
                let reason = format!(
                    "llama-server exited unexpectedly ({}): {}",
                    status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string()),
                    log_tail(&spec.log_file)
                );
                crashes.retain(|t| t.elapsed() < spec.restart_window);
                crashes.push(Instant::now());
                {
                    let mut i = info.lock().unwrap();
                    i.last_error = Some(reason.clone());
                    i.restarts += 1;
                }
                bus.emit("instance.crashed", Some(subject), json!({"reason": reason, "crashes_in_window": crashes.len()}));
                set(&info, &tx, InstanceState::Crashed);
                if crashes.len() as u32 > spec.max_restarts {
                    bus.emit("instance.failed", Some(subject), json!({"reason": "crashed repeatedly – giving up"}));
                    set(&info, &tx, InstanceState::Failed);
                    return;
                }
                let wait = spec.restart_backoff * crashes.len() as u32;
                tokio::select! {
                    _ = stop.cancelled() => {
                        bus.emit("instance.stopped", Some(subject), json!({}));
                        set(&info, &tx, InstanceState::Stopped);
                        return;
                    }
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn orphans_of_this_home_are_stopped_and_others_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let spawn = |key: &std::path::Path| {
            std::process::Command::new(ancilo_testkit::fake_llama_server_bin())
                .args(["--port", "0", "--api-key-file"])
                .arg(key)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut ours = spawn(&run.join("m.key"));
        std::fs::write(run.join("m.pid"), ours.id().to_string()).unwrap();
        // A pid file pointing at a process of another home is ignored.
        let other = dir.path().join("other.key");
        let mut theirs = spawn(&other);
        std::fs::write(run.join("x.pid"), theirs.id().to_string()).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let stopped = reap_orphans(&run);
        assert_eq!(stopped, vec![ours.id()]);
        assert!(ours.wait().is_ok());
        assert!(
            theirs.try_wait().unwrap().is_none(),
            "other process must keep running"
        );
        theirs.kill().ok();
        assert!(
            std::fs::read_dir(&run)
                .unwrap()
                .flatten()
                .all(|e| e.path().extension().is_none_or(|x| x != "pid"))
        );
    }

    use super::*;
    use ancilo_testkit::{FakeFile, FakeHf, TestHome, fake_llama_server_bin};

    fn spec(home: &TestHome, model: &Path, env: Vec<(String, String)>) -> LaunchSpec {
        LaunchSpec {
            binary: fake_llama_server_bin(),
            model_path: model.to_path_buf(),
            alias: "m".into(),
            ctx_tokens: 4096,
            gpu_layers: -1,
            threads: 4,
            embeddings: false,
            log_file: home.paths.logs_dir().join("m.log"),
            api_key: "secret".into(),
            api_key_file: home.path().join("m.key"),
            load_timeout: Duration::from_secs(10),
            env,
            max_restarts: 3,
            restart_window: Duration::from_secs(60),
            restart_backoff: Duration::from_millis(50),
            parallel: 1,
            priority: crate::resources::Priority::Normal,
        }
    }

    fn model_file(home: &TestHome) -> PathBuf {
        let p = home.scratch("models").join("m.gguf");
        std::fs::write(
            &p,
            ancilo_testkit::gguf::fake_gguf("llama", "m", 4096, 1000),
        )
        .unwrap();
        p
    }

    #[tokio::test]
    async fn starts_serves_and_stops() {
        let home = TestHome::new();
        let bus = EventBus::in_memory();
        let inst =
            Instance::spawn(spec(&home, &model_file(&home), vec![]), bus, "m".into()).unwrap();
        inst.wait_ready(Duration::from_secs(10)).await.unwrap();
        let url = inst.base_url().unwrap();
        let r: serde_json::Value = reqwest::get(format!("{url}/v1/models"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r["data"][0]["id"], "m");
        assert!(inst.info().load_ms.is_some());
        inst.stop().await;
        assert_eq!(inst.info().state, InstanceState::Stopped);
        assert!(reqwest::get(format!("{url}/health")).await.is_err());
    }

    // covers: M1-AC-05
    #[tokio::test]
    async fn restarts_after_a_crash() {
        let home = TestHome::new();
        let marker = home.path().join("crashed-once");
        let env = vec![
            ("FAKE_LLM_EXIT_AFTER_MS".into(), "300".into()),
            (
                "FAKE_LLM_CRASH_ONCE_MARKER".into(),
                marker.display().to_string(),
            ),
        ];
        let bus = EventBus::in_memory();
        let mut rx = bus.subscribe();
        let inst = Instance::spawn(spec(&home, &model_file(&home), env), bus, "m".into()).unwrap();
        let mut kinds = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !kinds.contains(&"instance.restarted".to_string()) && Instant::now() < deadline {
            if let Ok(Ok(e)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                kinds.push(e.kind);
            }
        }
        assert!(kinds.contains(&"instance.crashed".to_string()), "{kinds:?}");
        assert!(
            kinds.contains(&"instance.restarted".to_string()),
            "{kinds:?}"
        );
        inst.wait_ready(Duration::from_secs(5)).await.unwrap();
        assert_eq!(inst.info().restarts, 1);
        inst.stop().await;
    }

    #[tokio::test]
    async fn reports_failure_when_the_model_cannot_load() {
        let home = TestHome::new();
        let inst = Instance::spawn(
            spec(&home, Path::new("/nonexistent.gguf"), vec![]),
            EventBus::in_memory(),
            "m".into(),
        )
        .unwrap();
        let err = inst.wait_ready(Duration::from_secs(10)).await.unwrap_err();
        assert!(
            err.message().contains("failed to load model"),
            "{}",
            err.message()
        );
        assert_eq!(inst.info().state, InstanceState::Failed);
    }

    // covers: M9-AC-06
    #[test]
    fn the_shipped_llama_server_is_found_next_to_ancilo() {
        let dir = tempfile::tempdir().unwrap();
        // Archive / Homebrew layout: bin/ancilo + libexec/ancilo/llama-server.
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::create_dir_all(dir.path().join("libexec/ancilo")).unwrap();
        let exe = dir.path().join("bin/ancilo");
        assert_eq!(bundled_near(&exe), None);
        std::fs::write(dir.path().join("libexec/ancilo/llama-server"), "x").unwrap();
        assert!(
            bundled_near(&exe)
                .unwrap()
                .ends_with("libexec/ancilo/llama-server")
        );
        // App bundle: Contents/MacOS/{ancilo, llama-server} – takes precedence.
        std::fs::write(dir.path().join("bin/llama-server"), "x").unwrap();
        assert_eq!(
            bundled_near(&exe).unwrap(),
            dir.path().join("bin/llama-server")
        );
    }

    #[tokio::test]
    async fn installs_the_pinned_build_with_checksum() {
        let home = TestHome::new();
        // A tiny archive containing llama-bX/llama-server.
        let mut tar_gz = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_gz, flate2::Compression::fast());
            let mut b = tar::Builder::new(enc);
            let data = b"#!/bin/sh\necho fake\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "llama-bX/llama-server", &data[..])
                .unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let file = FakeFile::new(
            "ggml-org/llama.cpp/releases/download/bX/llama-test.tar.gz",
            tar_gz,
        );
        let build = PinnedBuild {
            tag: "bX",
            asset: "llama-test.tar.gz",
            sha256: Box::leak(file.sha256().into_boxed_str()),
            size: file.content.len() as u64,
        };
        let hf = FakeHf::start_with_raw(vec![], vec![file]).await;
        let config = Config {
            github_endpoint: hf.url(),
            ..home.config()
        };
        let bus = EventBus::in_memory();
        let http = reqwest::Client::new();
        // Downloads only when allowed.
        let err = ensure_binary(
            &home.paths,
            &Config {
                llama_auto_install: Some(false),
                ..config.clone()
            },
            &http,
            &bus,
            Some(build.clone()),
        )
        .await
        .unwrap_err();
        assert!(
            err.message().contains("ancilo llama install"),
            "{}",
            err.message()
        );
        assert!(hf.requests().is_empty());
        let bin = ensure_binary(&home.paths, &config, &http, &bus, Some(build.clone()))
            .await
            .unwrap();
        assert!(bin.ends_with("llama-bX/llama-server"));
        // Second call uses the installed copy.
        let again = ensure_binary(&home.paths, &config, &http, &bus, Some(build.clone()))
            .await
            .unwrap();
        assert_eq!(bin, again);
        assert_eq!(hf.requests().len(), 1);
        // A changed binary is noticed, not used.
        std::fs::write(&bin, b"#!/bin/sh\necho evil\n").unwrap();
        let err = ensure_binary(&home.paths, &config, &http, &bus, Some(build.clone()))
            .await
            .unwrap_err();
        assert!(
            err.message().contains("changed or damaged"),
            "{}",
            err.message()
        );
        // Reinstalling repairs it.
        install(&home.paths, &config, &http, &bus, build.clone())
            .await
            .unwrap();
        assert_eq!(
            ensure_binary(&home.paths, &config, &http, &bus, Some(build.clone()))
                .await
                .unwrap(),
            bin
        );

        // A wrong checksum is rejected.
        let home2 = TestHome::new();
        let bad = PinnedBuild {
            sha256: "00",
            ..build
        };
        let err = ensure_binary(&home2.paths, &config, &http, &bus, Some(bad))
            .await
            .unwrap_err();
        assert!(err.message().contains("checksum"));
    }
}
