//! The Ancilo daemon: one per home, long-running, loopback only.
//!
//! [`start`] wires storage, the event bus, the model manager and the HTTP
//! server together and returns a handle. The CLI runs it in the foreground
//! (`ancilo daemon run`); tests run it in-process.

pub mod docs;
mod knowledge;
pub mod preferences;
pub mod secrets;
pub mod system;
pub mod web;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ancilo_core::{Config, Error, EventBus, NoInput, OpBuilder, Paths, Registry, Result};
use ancilo_models::llama::PinnedBuild;
use ancilo_models::{ManagerOptions, ModelManager};
use ancilo_storage::Db;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Written to `daemon.json` so clients can find the running daemon.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    pub url: String,
    pub version: String,
    pub home: PathBuf,
    pub started_at: DateTime<Utc>,
}

impl DaemonInfo {
    pub fn read(paths: &Paths) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(paths.daemon_file()).ok()?).ok()
    }
}

/// Knobs for tests.
#[derive(Default)]
pub struct DaemonOptions {
    pub manager: ManagerOptions,
    pub tasks: ancilo_tasks::Options,
    /// `None`: the platform's pinned build.
    pub llama_build: Option<Option<PinnedBuild>>,
}

pub struct DaemonHandle {
    pub info: DaemonInfo,
    pub token: String,
    pub manager: ModelManager,
    pub gateway: ancilo_gateway::Gateway,
    pub tasks: ancilo_tasks::TaskRunner,
    pub comparer: ancilo_compare::Comparer,
    pub indexer: ancilo_index::Indexer,
    pub assistant: ancilo_assistant::Assistant,
    pub sessions: ancilo_sessions::Sessions,
    pub bus: EventBus,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    /// Watches memory, heat and idle models.
    guard: tokio::task::JoinHandle<()>,
    /// Held while the daemon runs: one daemon per home.
    _lock: std::fs::File,
}

impl DaemonHandle {
    pub fn url(&self) -> &str {
        &self.info.url
    }

    /// Resolves when the daemon was asked to shut down (e.g. `daemon_shutdown`).
    pub async fn wait_for_shutdown_request(&self) {
        self.shutdown.cancelled().await;
    }

    /// Stops the server and all model processes.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        // Open connections get a moment to finish; then the server stops.
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
        self.guard.abort();
        // Tasks first: they must not mistake stopped models for failures.
        self.tasks.shutdown().await;
        self.sessions.shutdown().await;
        self.comparer.shutdown().await;
        self.manager.shutdown().await;
        let path = Paths::from_home(&self.info.home).daemon_file();
        if DaemonInfo::read(&Paths::from_home(&self.info.home))
            .is_some_and(|i| i.pid == self.info.pid)
        {
            std::fs::remove_file(path).ok();
        }
    }
}

/// The access token for local clients, created on first start (mode 0600).
pub fn ensure_token(paths: &Paths) -> Result<String> {
    let file = paths.token_file();
    if let Ok(t) = std::fs::read_to_string(&file) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    std::fs::write(&file, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

#[derive(Debug, Serialize, JsonSchema)]
struct DaemonStatus {
    version: String,
    pid: u32,
    home: PathBuf,
    url: String,
    uptime_s: u64,
}

fn register_system_ops(
    registry: &mut Registry,
    info: DaemonInfo,
    started: Instant,
    shutdown: CancellationToken,
) {
    registry.register(
        OpBuilder::new("daemon_info")
            .summary("Show version, home directory and uptime of the Ancilo daemon")
            .handler(move |_ctx, _i: NoInput| {
                let info = info.clone();
                async move {
                    Ok(DaemonStatus {
                        version: info.version,
                        pid: info.pid,
                        home: info.home,
                        url: info.url,
                        uptime_s: started.elapsed().as_secs(),
                    })
                }
            }),
    );
    registry.register(
        OpBuilder::new("daemon_shutdown")
            .summary("Stop the Ancilo daemon and all loaded models")
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let s = shutdown.clone();
                async move {
                    s.cancel();
                    Ok("shutting down")
                }
            }),
    );
}

/// How the desktop app looks for updates. Checking is network traffic, so it
/// happens automatically only with the user's consent (M9, PRIVACY.md).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateSettings {
    /// Check for updates automatically (default: off – only on request).
    auto_check: bool,
}

const AUTO_CHECK: &str = "updates.auto_check";

fn register_update_settings(registry: &mut Registry, db: Db) {
    let d = db.clone();
    registry.register(
        OpBuilder::new("get_update_settings")
            .summary("Whether the app looks for updates automatically")
            .handler(move |_ctx, _i: NoInput| {
                let d = d.clone();
                async move {
                    Ok(UpdateSettings {
                        auto_check: d.get_setting(AUTO_CHECK)?.as_deref() == Some("true"),
                    })
                }
            }),
    );
    registry.register(
        OpBuilder::new("set_update_settings")
            .summary("Allow or stop automatic update checks of the app")
            .manage()
            .handler(move |_ctx, i: UpdateSettings| {
                let db = db.clone();
                async move {
                    db.set_setting(AUTO_CHECK, if i.auto_check { "true" } else { "false" })?;
                    Ok(i)
                }
            }),
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DelegationEvalInput {
    /// Built-in suite (`delegation`) or path to a YAML suite.
    #[serde(default)]
    suite: Option<String>,
    /// Model id or role (default: role delegation / default model).
    #[serde(default)]
    model: Option<String>,
    /// Runs per task (default 1).
    #[serde(default)]
    repeat: Option<u32>,
    /// Offer the worker the project search (default true).
    #[serde(default)]
    search: Option<bool>,
}

fn register_delegation_eval(
    registry: &mut Registry,
    gateway: ancilo_gateway::Gateway,
    url: String,
    token: String,
    dir: PathBuf,
) {
    registry.register(
        OpBuilder::new("run_delegation_eval")
            .summary("Run the delegation eval: real tasks on fixture repositories, checked automatically")
            .manage()
            .handler(move |_ctx, i: DelegationEvalInput| {
                let (gw, url, token, dir) = (gateway.clone(), url.clone(), token.clone(), dir.clone());
                async move {
                    let name = i.suite.unwrap_or_else(|| "delegation".into());
                    let suite = match ancilo_eval::delegation::builtin(&name) {
                        Some(s) => s,
                        None => serde_yaml_from(&name)?,
                    };
                    let model = gw.manager().resolve_name(i.model.as_deref().unwrap_or("delegation"))?;
                    let report = ancilo_eval::delegation::run(&suite, &url, &token, &model, i.repeat.unwrap_or(1), i.search.unwrap_or(true)).await?;
                    std::fs::create_dir_all(&dir)?;
                    let file = dir.join(format!("{}-{}-{}.json", suite.name, model, report.started_at.format("%Y%m%dT%H%M%S")));
                    std::fs::write(file, serde_json::to_string_pretty(&report)?)?;
                    Ok(report)
                }
            }),
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CodingEvalInput {
    /// `coding` (built in) or a path to a suite file.
    #[serde(default)]
    suite: Option<String>,
    /// Model id or role (default: role `coding`).
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    repeat: Option<u32>,
}

/// M8: reference tasks through the coding session operations the app uses.
fn register_coding_eval(
    registry: &mut Registry,
    gateway: ancilo_gateway::Gateway,
    url: String,
    token: String,
    dir: PathBuf,
) {
    registry.register(
        OpBuilder::new("run_coding_eval")
            .summary("Run the coding eval: tasks through coding sessions (as in the app), checked on the project")
            .manage()
            .handler(move |_ctx, i: CodingEvalInput| {
                let (gw, url, token, dir) = (gateway.clone(), url.clone(), token.clone(), dir.clone());
                async move {
                    let name = i.suite.unwrap_or_else(|| "coding".into());
                    let suite = match ancilo_eval::coding::builtin(&name) {
                        Some(s) => s,
                        None => {
                            let text = std::fs::read_to_string(&name).map_err(|e| Error::not_found(format!("{name}: {e}")))?;
                            serde_json::from_value(yaml_to_json(&text)?).map_err(|e| Error::invalid(format!("{name}: {e}")))?
                        }
                    };
                    let model = gw
                        .manager()
                        .route(&ancilo_models::routing::RouteRequest {
                            model: i.model.as_deref(),
                            role: ancilo_sessions::ROLE_CODING,
                            ..Default::default()
                        })?
                        .model;
                    let report = ancilo_eval::coding::run(&suite, &url, &token, &model, i.repeat.unwrap_or(1)).await?;
                    std::fs::create_dir_all(&dir)?;
                    let file = dir.join(format!("{}-{}-{}.json", suite.name, model, report.started_at.format("%Y%m%dT%H%M%S")));
                    std::fs::write(file, serde_json::to_string_pretty(&report)?)?;
                    Ok(report)
                }
            }),
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssistantEvalInput {
    /// Model for the assistant (default: role assistant / default model).
    #[serde(default)]
    model: Option<String>,
    /// A second chat model for tasks that assign or compare (default: any
    /// other local chat model).
    #[serde(default)]
    other_model: Option<String>,
}

/// Eval-Set III on this Ancilo: every task restores what it changed.
fn register_assistant_eval(
    registry: &mut Registry,
    assistant: ancilo_assistant::Assistant,
    cell: Arc<std::sync::OnceLock<Arc<Registry>>>,
    manager: ModelManager,
    scratch: PathBuf,
) {
    registry.register(
        OpBuilder::new("run_assistant_eval")
            .summary("Run Eval-Set III: requests to the assistant with checked target states")
            .manage()
            .handler(move |_ctx, i: AssistantEvalInput| {
                let (a, cell, m, scratch) = (
                    assistant.clone(),
                    cell.clone(),
                    manager.clone(),
                    scratch.clone(),
                );
                async move {
                    let registry = cell
                        .get()
                        .cloned()
                        .ok_or_else(|| Error::unavailable("not ready"))?;
                    let suite = ancilo_assistant::eval::builtin("assistant")
                        .ok_or_else(|| Error::internal("suite missing"))?;
                    let default = m.resolve_strict("default")?;
                    let models = m.list().await?;
                    let other = match i.other_model {
                        Some(o) => m.resolve_strict(&o)?,
                        None => models
                            .iter()
                            .find(|x| !x.embedding && !x.cloud && x.id != default)
                            .map(|x| x.id.clone())
                            .ok_or_else(|| {
                                Error::invalid("the eval needs a second local chat model")
                            })?,
                    };
                    if other == default {
                        return Err(Error::invalid(
                            "other_model must differ from the default model (A/B tests need two models)",
                        ));
                    }
                    let assistant_model = match &i.model {
                        Some(x) => m.resolve_strict(x)?,
                        None => m.resolve_name("assistant")?,
                    };
                    // A model the assistant may stop without stopping itself.
                    let idle = [other.clone(), default.clone()]
                        .into_iter()
                        .find(|x| *x != assistant_model)
                        .unwrap_or_else(|| other.clone());
                    let embed = m
                        .embedding_model()
                        .ok_or_else(|| Error::invalid("the eval needs an embedding model"))?;
                    let project = scratch.join(format!("assistant-eval-{}", std::process::id()));
                    std::fs::create_dir_all(project.join("src"))?;
                    std::fs::create_dir_all(project.join(".git"))?;
                    std::fs::write(
                        project.join("src/lib.rs"),
                        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
                    )?;
                    let cwd = std::fs::canonicalize(&project)?.display().to_string();
                    // Answers may name a model by id or by its display name.
                    let other_name = models
                        .iter()
                        .find(|x| x.id == other)
                        .map(|x| x.name.clone())
                        .unwrap_or_else(|| other.clone());
                    let vars = [
                        ("model", other.as_str()),
                        ("model_name", other_name.as_str()),
                        ("default", default.as_str()),
                        ("idle", idle.as_str()),
                        ("embed", embed.as_str()),
                        ("cwd", cwd.as_str()),
                    ];
                    let report = ancilo_assistant::eval::run(
                        &a,
                        &registry,
                        &suite,
                        i.model.as_deref(),
                        &vars,
                    )
                    .await;
                    std::fs::remove_dir_all(&project).ok();
                    report
                }
            }),
    );
}

fn serde_yaml_from(path: &str) -> Result<ancilo_eval::delegation::Suite> {
    let text =
        std::fs::read_to_string(path).map_err(|e| Error::not_found(format!("{path}: {e}")))?;
    serde_json::from_value(yaml_to_json(&text)?).map_err(|e| Error::invalid(format!("{path}: {e}")))
}

fn yaml_to_json(text: &str) -> Result<serde_json::Value> {
    ancilo_eval::yaml_value(text)
}

fn clients(paths: &Paths, config: &Config) -> ancilo_connect::Clients {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    ancilo_connect::Clients {
        claude_bin: config.claude_bin.clone().unwrap_or_else(|| "claude".into()),
        codex_bin: config.codex_bin.clone().unwrap_or_else(|| "codex".into()),
        ancilo_bin: config
            .ancilo_bin
            .clone()
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| "ancilo".into()),
        dir: paths.home().join("clients"),
        codex_home: config
            .codex_home
            .clone()
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".codex")),
        env: vec![("ANCILO_HOME".into(), paths.home().display().to_string())],
    }
}

/// Starts the daemon. Returns once the server accepts connections.
/// Takes the home's lock: two daemons on one home would manage the same
/// models and database concurrently.
fn lock_home(paths: &Paths) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.home().join("daemon.lock"))?;
    // A daemon that just stopped may release the lock a moment later.
    for attempt in 0..20 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if attempt < 19 => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    Err(Error::Conflict(format!(
        "another Ancilo daemon is running for {} – stop it first (`ancilo daemon stop`)",
        paths.home().display()
    )))
}

pub async fn start(
    paths: Paths,
    config: Config,
    mut options: DaemonOptions,
) -> Result<DaemonHandle> {
    paths.ensure()?;
    let lock = lock_home(&paths)?;
    let db = Db::open(&paths.db_file())?;
    let bus = EventBus::new(Some(Arc::new(db.clone())), db.last_event_seq()?);
    let hw = ancilo_models::hardware::detect(&config, paths.home())?;
    let llama_build = options
        .llama_build
        .unwrap_or_else(ancilo_models::llama::pinned);
    let manager = ModelManager::new(
        paths.clone(),
        config.clone(),
        db.clone(),
        bus.clone(),
        hw,
        llama_build,
        options.manager,
    );
    let token = ensure_token(&paths)?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.port))
        .await
        .map_err(|e| {
            Error::Conflict(format!(
                "cannot listen on 127.0.0.1:{} ({e}) – is another Ancilo daemon running?",
                config.port
            ))
        })?;
    let port = listener.local_addr()?.port();
    let info = DaemonInfo {
        pid: std::process::id(),
        port,
        url: format!("http://127.0.0.1:{port}"),
        version: ancilo_core::VERSION.into(),
        home: paths.home().to_path_buf(),
        started_at: Utc::now(),
    };
    let shutdown = CancellationToken::new();
    let started = Instant::now();
    let gateway = ancilo_gateway::Gateway::new(manager.clone(), db.clone(), bus.clone());
    let indexer = ancilo_index::Indexer::new(
        paths.index_dir(),
        bus.clone(),
        Arc::new(ancilo_index::GatewayEmbedder(gateway.clone())),
    );
    let search: Arc<dyn ancilo_agent::CodeSearch> =
        Arc::new(knowledge::IndexSearch(indexer.clone()));
    // Commands of agents never read Ancilo's home (other sessions, key file).
    options.tasks.shell.hidden.push(paths.home().to_path_buf());
    let comparer = ancilo_compare::Comparer::new(
        db.clone(),
        bus.clone(),
        gateway.clone(),
        paths.clone(),
        options.tasks.shell.clone(),
        Some(search.clone()),
    );
    let terminals = ancilo_sessions::Terminals::new(bus.clone());
    // Documents are read by the `ancilo` program itself, in a sandboxed
    // process; a test daemon (another program) reads them in-process.
    let reader = config.ancilo_bin.clone().or_else(|| {
        std::env::current_exe()
            .ok()
            .filter(|p| p.file_name().is_some_and(|n| n == "ancilo"))
    });
    let extractor = Arc::new(ancilo_docs::Extractor::new(
        reader,
        paths.home().join("tmp").join("documents"),
        vec![paths.home().to_path_buf()],
    ));
    let web_search = web::WebSearch::new(&config, db.clone(), manager.secrets(), bus.clone());
    let sessions = ancilo_sessions::Sessions::new(
        db.clone(),
        bus.clone(),
        gateway.clone(),
        &paths,
        options.tasks.shell.clone(),
        Some(search.clone()),
        terminals.clone(),
    )
    .with_projects_dir(config.projects_dir.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| paths.home().to_path_buf())
            .join("Ancilo")
    }))
    .with_web(Arc::new(web::AgentWeb(web_search.clone())))
    .with_documents(extractor.clone());
    let mut task_options = options.tasks;
    task_options.search.get_or_insert(search);
    let tasks = ancilo_tasks::TaskRunner::new(
        db.clone(),
        bus.clone(),
        gateway.clone(),
        paths.clone(),
        task_options,
    );
    let knowledge = Arc::new(knowledge::Knowledge {
        indexer: indexer.clone(),
        manager: manager.clone(),
        comparer: comparer.clone(),
        evals_dir: paths.home().join("evals"),
        cards_dir: paths.home().join("knowledge").join("model-cards"),
    });
    let mut registry = Registry::new();
    ancilo_models::ops::register(&mut registry, manager.clone());
    ancilo_models::routing::register(&mut registry, manager.clone());
    ancilo_compare::ops::register(&mut registry, comparer.clone());
    ancilo_index::ops::register(&mut registry, indexer.clone(), paths.home().join("tmp"));
    knowledge::register(&mut registry, knowledge.clone());
    system::register(
        &mut registry,
        manager.clone(),
        gateway.clone(),
        db.clone(),
        config.clone(),
        paths.logs_dir(),
    );
    let attachments = ancilo_docs::Attachments::new(db.clone(), extractor.clone());
    if let Err(e) = attachments.prune() {
        tracing::warn!(error = %e.message(), "removing unsent attachments failed");
    }
    let library =
        ancilo_docs::library::Library::new(db.clone(), attachments.extractor(), Some(bus.clone()));
    docs::register(&mut registry, attachments.clone(), library.clone());
    let assistant = ancilo_assistant::Assistant::new(gateway.clone(), bus.clone(), db.clone())
        .with_documents(attachments.clone())
        .with_library(library.clone());
    ancilo_assistant::ops::register(&mut registry, assistant.clone());
    ancilo_sessions::ops::register(&mut registry, sessions.clone());
    let registry_cell: Arc<std::sync::OnceLock<Arc<Registry>>> = Arc::default();
    register_assistant_eval(
        &mut registry,
        assistant.clone(),
        registry_cell.clone(),
        manager.clone(),
        paths.home().join("tmp"),
    );
    ancilo_tasks::ops::register(&mut registry, tasks.clone());
    ancilo_connect::register(&mut registry, clients(&paths, &config));
    register_update_settings(&mut registry, db.clone());
    preferences::register(
        &mut registry,
        db.clone(),
        bus.clone(),
        Some(library.clone()),
    );
    // Chat projects: what changed in their folders is read in the background.
    for folder in preferences::load(&db).documents {
        library.refresh_soon(&folder);
    }
    web::register(&mut registry, web_search);
    register_delegation_eval(
        &mut registry,
        gateway.clone(),
        info.url.clone(),
        token.clone(),
        paths.home().join("evals"),
    );
    register_coding_eval(
        &mut registry,
        gateway.clone(),
        info.url.clone(),
        token.clone(),
        paths.home().join("evals"),
    );
    ancilo_gateway::ops::register(
        &mut registry,
        gateway.clone(),
        info.url.clone(),
        token.clone(),
        paths.home().join("evals"),
    );
    register_system_ops(&mut registry, info.clone(), started, shutdown.clone());

    let registry = Arc::new(registry);
    assistant.attach(registry.clone());
    let _ = registry_cell.set(registry.clone());
    let state = ancilo_server::AppState {
        registry,
        token: Arc::new(token.clone()),
        bus: bus.clone(),
        db,
        started,
        version: ancilo_core::VERSION,
        shutdown: shutdown.clone(),
    };
    let pty = axum::Router::new()
        .route(
            "/api/v1/pty/{id}",
            axum::routing::get(ancilo_sessions::pty::websocket),
        )
        .with_state(terminals);
    let app = ancilo_server::router_with(
        state,
        ancilo_gateway::routes::router(gateway.clone())
            .merge(ancilo_mcp::router())
            .merge(docs::routes(attachments))
            .merge(ancilo_sessions::ops::files_route(sessions.clone())),
        pty,
    );
    let stop = shutdown.clone();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await });
        if let Err(e) = server.await {
            tracing::error!(error = %e, "server stopped with an error");
        }
    });
    std::fs::write(paths.daemon_file(), serde_json::to_string_pretty(&info)?)?;
    bus.emit(
        "daemon.started",
        None,
        serde_json::json!({"port": port, "version": info.version}),
    );
    manager.restore().await?;
    let guard = manager.spawn_guard();
    tasks.restore().await?;
    comparer.restore()?;
    sessions.restore()?;
    // Knowledge base and index housekeeping in the background; afterwards the
    // knowledge base follows new eval results, comparisons and models. Start
    // only re-indexes what is on disk: Hugging Face is contacted for a model
    // card only when the user adds a model (guardrail "local first").
    {
        let (k, ix, mut events) = (knowledge.clone(), indexer.clone(), bus.subscribe());
        tokio::spawn(async move {
            let removed = tokio::task::spawn_blocking(move || ix.collect_garbage())
                .await
                .unwrap_or(0);
            if removed > 0 {
                tracing::info!(removed, "removed indexes of deleted projects");
            }
            if let Err(e) = k.refresh(false).await {
                tracing::info!(error = %e.message(), "knowledge base not refreshed");
            }
            loop {
                match events.recv().await {
                    Ok(e) if e.kind == "eval.finished" || e.kind == "compare.finished" => {
                        k.refresh(false).await.ok();
                    }
                    // The user just added a model (from Hugging Face, which
                    // was contacted for it anyway): its card joins the base.
                    Ok(e) if e.kind == "model.added" => {
                        let Some(id) = e.subject.as_deref() else {
                            continue;
                        };
                        match k.fetch_card(id).await {
                            Ok(true) => {
                                k.refresh(false).await.ok();
                            }
                            Ok(false) => {}
                            Err(e) => {
                                tracing::info!(model = %id, error = %e.message(), "model card not fetched");
                            }
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }
    Ok(DaemonHandle {
        info,
        token,
        manager,
        gateway,
        tasks,
        comparer,
        indexer,
        assistant,
        sessions,
        bus,
        shutdown,
        task,
        guard,
        _lock: lock,
    })
}

/// Runs the daemon until SIGINT/SIGTERM or a `daemon_shutdown` request.
pub async fn run(paths: Paths, config: Config) -> Result<()> {
    let options = DaemonOptions {
        manager: ManagerOptions {
            secrets: Arc::new(secrets::KeyringSecrets::new("ancilo")),
            ..Default::default()
        },
        ..Default::default()
    };
    let handle = start(paths, config, options).await?;
    tracing::info!(url = %handle.info.url, "Ancilo daemon running");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = handle.wait_for_shutdown_request() => {}
    }
    tracing::info!("Ancilo daemon stopping");
    handle.stop().await;
    Ok(())
}
