//! The model manager: the model library, downloads and running instances.
//!
//! Every public method backs an operation (see [`crate::ops`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_core::{Config, Error, EventBus, Paths, Result};
use ancilo_net::{By, Note, Purpose as NetPurpose};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::{OptionalExtension, params};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::address::{self, Address};
use crate::catalog::{self, Catalog, CatalogInfo, Installed, Purpose, Recommendations};
use crate::discovery::{self, FoundIn};
use crate::download::{self, DownloadOptions, DownloadSpec};
use crate::gguf::{self, GgufMeta};
use crate::hardware::{Gpu, HardwareProfile};
use crate::health::{self, Consumer, Health, HealthLevel, Sampler};
use crate::hf::{HfClient, RepoInfo, SearchHit};
use crate::llama::{self, Instance, InstanceState, LaunchSpec, PinnedBuild};
use crate::planner::{self, ContextSize, Fit, ModelShape, Plan, RepoFile, Wish};
use crate::resources::{self, Loaded, Reason, ResourceSettings, SystemState};

/// Where a model came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelSource {
    HuggingFace {
        repo: String,
        revision: String,
    },
    Local,
    /// Served by an OpenAI-compatible server (Ollama, LM Studio, a cloud
    /// provider). `base_url` is the API base (`…/v1`); its key, if any, is in
    /// the secret store.
    Remote {
        base_url: String,
        model: String,
        /// Not on this machine (the host is not loopback).
        cloud: bool,
    },
}

/// Where to send requests for a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// API base, e.g. `http://127.0.0.1:41234/v1`.
    pub base: String,
    /// Bearer token (may be empty).
    pub key: String,
    /// Model name the server expects.
    pub model: String,
    pub cloud: bool,
}

/// Name of a model's API key in the secret store.
pub fn secret_name(model_id: &str) -> String {
    format!("model:{model_id}")
}

fn is_loopback_url(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', ':']).next().unwrap_or("");
    let host = if rest.starts_with('[') {
        rest.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host
    };
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Downloading,
    Ready,
    Failed,
}

/// A model in the library (persisted).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModelRecord {
    pub id: String,
    pub name: String,
    pub source: ModelSource,
    /// Local paths of all parts, in order.
    pub files: Vec<PathBuf>,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub quant: Option<String>,
    pub state: FileState,
    pub failure: Option<String>,
    pub plan: Plan,
    pub meta: Option<GgufMeta>,
    /// An embedding model (serves `/v1/embeddings`, not chat).
    pub embedding: bool,
    /// The file belongs to another tool (LM Studio, Ollama, …) and is used in
    /// place. Ancilo never deletes such files.
    pub found_in: Option<FoundIn>,
    /// Start again when the daemon starts.
    pub autostart: bool,
    /// Never unloaded to make room for other models.
    #[serde(default)]
    pub pinned: bool,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    Downloading,
    DownloadFailed,
    /// On disk, not loaded.
    Ready,
    Starting,
    Running,
    Crashed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DownloadView {
    pub bytes: u64,
    pub total: Option<u64>,
    pub percent: Option<f64>,
    pub bytes_per_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstanceView {
    pub port: Option<u16>,
    pub load_ms: Option<u64>,
    pub restarts: u32,
    pub tokens_per_sec: Option<f64>,
    pub last_error: Option<String>,
}

/// What clients see of a model.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModelView {
    pub id: String,
    pub name: String,
    pub quant: Option<String>,
    pub size_bytes: u64,
    /// Human description of the origin.
    pub source: String,
    pub path: Option<PathBuf>,
    pub status: ModelStatus,
    pub failure: Option<String>,
    pub roles: Vec<String>,
    pub embedding: bool,
    /// Stays loaded (never unloaded to make room).
    #[serde(default)]
    pub pinned: bool,
    /// Runs at a provider outside this machine.
    #[serde(default)]
    pub cloud: bool,
    pub ctx_tokens: u64,
    pub expected_ram_bytes: u64,
    pub fit: Fit,
    pub download: Option<DownloadView>,
    pub instance: Option<InstanceView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanPreview {
    pub address: Address,
    pub repo: Option<RepoInfo>,
    pub plan: Plan,
    /// A local copy that would be used instead of downloading.
    pub existing: Option<PathBuf>,
    pub existing_in: Option<FoundIn>,
    /// Bytes that would be downloaded.
    pub download_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HardwareView {
    pub hardware: HardwareProfile,
    /// Memory models may use in total (GPU limit, minus the reserve).
    pub model_budget_bytes: u64,
    /// Taken by loaded models.
    pub used_bytes: u64,
    pub free_for_models_bytes: u64,
}

/// Models and role assignments (`role`, `model id`).
pub struct ModelNames {
    pub models: Vec<ModelRecord>,
    pub roles: Vec<(String, String)>,
}

pub const ROLE_DEFAULT: &str = "default";
pub const ROLE_EMBED: &str = "embed";

/// Tunables (tests shorten timeouts).
#[derive(Debug, Clone)]
pub struct ManagerOptions {
    pub download: DownloadOptions,
    pub restart_backoff: Duration,
    pub measure_speed: bool,
    /// API keys of remote models (the system keychain in production).
    pub secrets: Arc<dyn ancilo_core::secrets::SecretStore>,
    /// How often the resource guard looks at the computer.
    pub guard_interval: Duration,
    /// The user's log of what left this computer (downloads, Hugging Face,
    /// the catalog, cloud models).
    pub outbound: Option<Arc<dyn ancilo_net::Recorder>>,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            download: DownloadOptions::default(),
            restart_backoff: Duration::from_secs(2),
            measure_speed: true,
            secrets: Arc::new(ancilo_core::secrets::MemorySecrets::default()),
            guard_interval: Duration::from_secs(10),
            outbound: None,
        }
    }
}

struct Inner {
    paths: Paths,
    config: Config,
    db: Db,
    bus: EventBus,
    hw: HardwareProfile,
    hf: HfClient,
    http: ancilo_net::Net,
    llama_build: Option<PinnedBuild>,
    options: ManagerOptions,
    instances: tokio::sync::Mutex<HashMap<String, Arc<Instance>>>,
    downloads: Mutex<HashMap<String, (CancellationToken, DownloadView)>>,
    speeds: Mutex<HashMap<String, f64>>,
    binary: tokio::sync::Mutex<Option<PathBuf>>,
    /// Last request per model (least-recently-used eviction).
    last_used: Mutex<HashMap<String, Instant>>,
    /// Requests in flight per model; busy models are never evicted.
    busy: Mutex<HashMap<String, usize>>,
    /// Models an agent is working with (see [`ModelManager::begin_work`]).
    working: Mutex<HashMap<String, usize>>,
    /// Models coming back as they ran a moment ago (a larger context did not
    /// start): only an emergency keeps them from it.
    restoring: Mutex<HashSet<String>>,
    /// Serialises on-demand loading so two requests do not evict each other.
    loading: tokio::sync::Mutex<()>,
    /// How much of the computer Ancilo may take.
    resources: Mutex<ResourceSettings>,
    /// What the guard saw last and did recently.
    guard: Mutex<GuardLog>,
    /// Processor and program measurements (the system monitor).
    sampler: Mutex<Option<Sampler>>,
    /// The last measured state, reused for a moment (several views ask at once).
    measured: Mutex<Option<(Instant, SystemState)>>,
}

const RESOURCES_KEY: &str = "resources";
const ACTIVITY_MONITOR: &str = "/System/Applications/Utilities/Activity Monitor.app";

#[derive(Debug, Clone, Default)]
struct GuardLog {
    state: Option<SystemState>,
    health: Option<HealthLevel>,
    actions: std::collections::VecDeque<GuardAction>,
}

/// Something the resource guard did.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GuardAction {
    pub at: DateTime<Utc>,
    pub model: String,
    pub reason: Reason,
}

/// A loaded model in the resource view.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LoadedView {
    pub id: String,
    pub name: String,
    pub ram_bytes: u64,
    /// Answering right now.
    pub busy: bool,
    pub pinned: bool,
    pub idle_secs: u64,
    /// Unloaded after this many seconds without use (None: stays).
    pub unload_in_secs: Option<u64>,
}

/// The resource cockpit: settings, the computer's state, what Ancilo uses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResourceStatus {
    pub settings: ResourceSettings,
    /// The four levels as they would be set.
    pub presets: Vec<ResourceSettings>,
    pub system: SystemState,
    pub total_bytes: u64,
    /// What Ancilo's models may use at most (the level's share, within the
    /// GPU limit and the reserve for the system).
    pub cap_bytes: u64,
    pub used_bytes: u64,
    pub loaded: Vec<LoadedView>,
    /// Recent unloads by the guard, newest first.
    pub recent: Vec<GuardAction>,
}

/// Marks a model as in use while alive (see [`ModelManager::begin_use`]).
pub struct UseGuard {
    inner: Arc<Inner>,
    id: String,
}

/// An agent works with a model – see [`ModelManager::begin_work`].
pub struct WorkGuard {
    inner: Arc<Inner>,
    id: String,
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut working = self.inner.working.lock().unwrap();
        if let Some(n) = working.get_mut(&self.id) {
            *n = n.saturating_sub(1);
        }
        self.inner
            .last_used
            .lock()
            .unwrap()
            .insert(self.id.clone(), Instant::now());
    }
}

impl Drop for UseGuard {
    fn drop(&mut self) {
        let mut busy = self.inner.busy.lock().unwrap();
        if let Some(n) = busy.get_mut(&self.id) {
            *n = n.saturating_sub(1);
        }
        self.inner
            .last_used
            .lock()
            .unwrap()
            .insert(self.id.clone(), Instant::now());
    }
}

#[derive(Clone)]
pub struct ModelManager {
    inner: Arc<Inner>,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Memory, in binary units (how RAM is marketed and shown by macOS).
fn mem(b: u64) -> String {
    format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
}

/// File and disk sizes, in decimal units (as Finder shows them).
fn disk(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1e9)
}

fn looks_like_embedding(name: &str, meta: Option<&GgufMeta>) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("embed")
        || lower.contains("bge-")
        || meta.is_some_and(|m| m.pooling_type.is_some_and(|p| p > 0))
}

impl ModelManager {
    pub fn new(
        paths: Paths,
        config: Config,
        db: Db,
        bus: EventBus,
        hw: HardwareProfile,
        llama_build: Option<PinnedBuild>,
        options: ManagerOptions,
    ) -> Self {
        let hf = HfClient::new(
            &config.hf_endpoint,
            options.outbound.clone(),
            &config.web_hosts,
        );
        let http = ancilo_net::Net::new(
            ancilo_net::with_hosts(reqwest::Client::builder(), &config.web_hosts)
                .user_agent(crate::hf::user_agent())
                .connect_timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
            options.outbound.clone(),
        );
        Self {
            inner: Arc::new(Inner {
                paths,
                config,
                db,
                bus,
                hw,
                hf,
                http,
                llama_build,
                options,
                instances: Default::default(),
                downloads: Default::default(),
                speeds: Default::default(),
                binary: Default::default(),
                last_used: Default::default(),
                busy: Default::default(),
                working: Default::default(),
                restoring: Default::default(),
                loading: Default::default(),
                resources: Mutex::new(ResourceSettings::default()),
                guard: Mutex::new(GuardLog::default()),
                sampler: Mutex::new(None),
                measured: Mutex::new(None),
            }),
        }
        .with_saved_resources()
    }

    fn with_saved_resources(self) -> Self {
        let saved = self
            .inner
            .db
            .get_setting(RESOURCES_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<ResourceSettings>(&s).ok())
            .filter(|s| s.validate().is_ok());
        if let Some(s) = saved {
            *self.inner.resources.lock().unwrap() = s;
        }
        self
    }

    // ---- resources ----------------------------------------------------------

    /// Where API keys are kept (the system keychain in the daemon).
    /// The user's log of what left this computer (shared with the gateway and the web search).
    pub fn outbound(&self) -> Option<Arc<dyn ancilo_net::Recorder>> {
        self.inner.options.outbound.clone()
    }

    /// Host names reached at these addresses (tests: fakes out there).
    pub fn hosts(&self) -> &std::collections::BTreeMap<String, std::net::IpAddr> {
        &self.inner.config.web_hosts
    }

    pub fn secrets(&self) -> Arc<dyn ancilo_core::secrets::SecretStore> {
        self.inner.options.secrets.clone()
    }

    pub fn resource_settings(&self) -> ResourceSettings {
        self.inner.resources.lock().unwrap().clone()
    }

    /// Sets how much Ancilo may take: a level, or single values (then
    /// "custom"). Applies to models loaded from now on; the guard applies the
    /// new limits to loaded ones at its next look.
    pub fn set_resources(&self, settings: ResourceSettings) -> Result<ResourceSettings> {
        settings.validate().map_err(Error::invalid)?;
        let mut s = settings;
        s.custom = ResourceSettings {
            custom: false,
            ..s.clone()
        } != ResourceSettings::preset(s.level);
        self.inner
            .db
            .set_setting(RESOURCES_KEY, &serde_json::to_string(&s)?)?;
        *self.inner.resources.lock().unwrap() = s.clone();
        self.inner.bus.emit(
            "resources.changed",
            None,
            json!({"level": s.level, "custom": s.custom}),
        );
        Ok(s)
    }

    /// The computer right now (unknown when the hardware is overridden in
    /// tests, unless a probe file stands in for it).
    pub fn system_state(&self) -> SystemState {
        match (
            &self.inner.config.system_probe_override,
            &self.inner.config.hardware_override,
        ) {
            (Some(p), _) => resources::probe(Some(p)),
            (None, Some(_)) => SystemState::unknown(),
            (None, None) => {
                let mut measured = self.inner.measured.lock().unwrap();
                if let Some((at, s)) = measured.as_ref()
                    && at.elapsed() < Duration::from_secs(2)
                {
                    return s.clone();
                }
                let mut state = resources::probe(None);
                let (now, sustained) = self
                    .inner
                    .sampler
                    .lock()
                    .unwrap()
                    .get_or_insert_with(Sampler::new)
                    .cpu();
                state.cpu_percent = now;
                state.cpu_sustained_percent = sustained;
                *measured = Some((Instant::now(), state.clone()));
                state
            }
        }
    }

    /// All programs and what they take (tests: the probe file's `programs`).
    fn programs(&self) -> Vec<Consumer> {
        #[derive(Deserialize)]
        struct ProbeFile {
            #[serde(default)]
            programs: Vec<Consumer>,
        }
        match (
            &self.inner.config.system_probe_override,
            &self.inner.config.hardware_override,
        ) {
            (Some(p), _) => std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<ProbeFile>(&t).ok())
                .map(|f| f.programs)
                .unwrap_or_default(),
            (None, Some(_)) => Vec::new(),
            (None, None) => self
                .inner
                .sampler
                .lock()
                .unwrap()
                .get_or_insert_with(Sampler::new)
                .programs(),
        }
    }

    /// Whether this computer has an Activity Monitor to open.
    fn has_activity_monitor() -> bool {
        cfg!(target_os = "macos") && Path::new(ACTIVITY_MONITOR).exists()
    }

    /// The system monitor's verdict: is the computer about to be saturated,
    /// by whom, and what helps. Programs are only read while it is tight.
    pub async fn health(&self) -> Result<Health> {
        let settings = self.resource_settings();
        let state = self.system_state();
        let loaded = self.loaded().await;
        let own_bytes = self.used_bytes(None).await?;
        let idle_bytes = loaded
            .iter()
            .filter(|(m, _, _)| !m.busy)
            .map(|(_, ram, _)| ram)
            .sum();
        let total = self.inner.hw.total_ram_bytes;
        let monitor = Self::has_activity_monitor();
        let mut own = health::Own {
            bytes: own_bytes,
            idle_bytes,
            cpu_percent: 0.0,
        };
        let first = health::assess(&state, total, &settings, &own, &[], monitor);
        if first.level == HealthLevel::Ok {
            return Ok(first);
        }
        let programs = self.programs();
        own.cpu_percent = programs
            .iter()
            .filter(|p| p.ancilo)
            .map(|p| p.cpu_percent)
            .sum();
        Ok(health::assess(
            &state, total, &settings, &own, &programs, monitor,
        ))
    }

    /// Opens the Activity Monitor, where the user can close other programs
    /// (not in tests: there it only reports what it would open).
    pub fn open_activity_monitor(&self) -> Result<bool> {
        if !Self::has_activity_monitor() {
            return Err(Error::invalid("this computer has no Activity Monitor"));
        }
        if self.inner.config.system_probe_override.is_some()
            || self.inner.config.hardware_override.is_some()
        {
            return Ok(false);
        }
        std::process::Command::new("/usr/bin/open")
            .arg(ACTIVITY_MONITOR)
            .spawn()
            .map_err(Error::internal)?;
        Ok(true)
    }

    async fn loaded(&self) -> Vec<(Loaded, u64, String)> {
        let instances: Vec<(String, Arc<Instance>)> = self
            .inner
            .instances
            .lock()
            .await
            .iter()
            .filter(|(_, i)| {
                !matches!(
                    i.info().state,
                    InstanceState::Stopped | InstanceState::Failed
                )
            })
            .map(|(id, i)| (id.clone(), i.clone()))
            .collect();
        let busy = self.inner.busy.lock().unwrap().clone();
        let working = self.inner.working.lock().unwrap().clone();
        let last = self.inner.last_used.lock().unwrap().clone();
        let mut out = Vec::new();
        for (id, instance) in instances {
            let record = self.record(&id).ok().flatten();
            let since_start = (Utc::now() - instance.info().started_at)
                .num_seconds()
                .max(0) as u64;
            // Since its last request – or since it was loaded.
            let idle_secs = last
                .get(&id)
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(since_start)
                .min(since_start);
            out.push((
                Loaded {
                    busy: busy.get(&id).copied().unwrap_or(0) > 0,
                    working: working.get(&id).copied().unwrap_or(0) > 0,
                    pinned: record.as_ref().is_some_and(|r| r.pinned),
                    idle_secs,
                    id: id.clone(),
                },
                record.as_ref().map_or(0, |r| r.plan.expected_ram_bytes),
                record.map_or(id, |r| r.name),
            ));
        }
        out
    }

    pub async fn resource_status(&self) -> Result<ResourceStatus> {
        let settings = self.resource_settings();
        let system = self.system_state();
        let loaded = self.loaded().await;
        let recent = self
            .inner
            .guard
            .lock()
            .unwrap()
            .actions
            .iter()
            .cloned()
            .collect();
        Ok(ResourceStatus {
            presets: [
                resources::Level::Eco,
                resources::Level::Balanced,
                resources::Level::Performance,
                resources::Level::Max,
            ]
            .into_iter()
            .map(ResourceSettings::preset)
            .collect(),
            total_bytes: self.inner.hw.total_ram_bytes,
            cap_bytes: self.model_budget(),
            used_bytes: self.used_bytes(None).await?,
            loaded: loaded
                .into_iter()
                .map(|(m, ram, name)| LoadedView {
                    unload_in_secs: resources::unload_in(&settings, &m),
                    id: m.id,
                    name,
                    ram_bytes: ram,
                    busy: m.busy || m.working,
                    pinned: m.pinned,
                    idle_secs: m.idle_secs,
                })
                .collect(),
            recent,
            system,
            settings,
        })
    }

    /// Unloads every model that is not answering right now.
    pub async fn unload_all(&self) -> Result<Vec<String>> {
        let mut done = Vec::new();
        for (m, _, _) in self.loaded().await {
            if !m.busy {
                self.stop(&m.id).await?;
                done.push(m.id);
            }
        }
        Ok(done)
    }

    /// One look of the resource guard: unloads idle models, and models when
    /// memory gets short or the computer hot (per the settings).
    pub async fn guard_once(&self) -> Vec<GuardAction> {
        let settings = self.resource_settings();
        let state = self.system_state();
        let changed = {
            let mut g = self.inner.guard.lock().unwrap();
            let changed = g
                .state
                .as_ref()
                .is_none_or(|s| s.pressure != state.pressure || s.thermal != state.thermal);
            g.state = Some(state.clone());
            changed
        };
        if changed {
            self.inner.bus.emit(
                "resources.state",
                None,
                json!({"pressure": state.pressure, "thermal": state.thermal}),
            );
        }
        // The system monitor: a change of the verdict reaches the app at once.
        if let Ok(h) = self.health().await {
            let before = self.inner.guard.lock().unwrap().health.replace(h.level);
            if before != Some(h.level) {
                self.inner.bus.emit(
                    "system.health",
                    None,
                    json!({"level": h.level, "causes": h.causes}),
                );
            }
        }
        let loaded: Vec<Loaded> = self.loaded().await.into_iter().map(|(m, _, _)| m).collect();
        let mut actions = Vec::new();
        for (id, reason) in resources::to_unload(&settings, &state, &loaded) {
            if self.stop(&id).await.is_err() {
                continue;
            }
            let action = GuardAction {
                at: Utc::now(),
                model: id.clone(),
                reason,
            };
            self.inner
                .bus
                .emit("model.unloaded", Some(&id), json!({"reason": reason}));
            let mut g = self.inner.guard.lock().unwrap();
            g.actions.push_front(action.clone());
            g.actions.truncate(20);
            actions.push(action);
        }
        actions
    }

    /// Runs the resource guard until the manager is dropped.
    pub fn spawn_guard(&self) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        let interval = self.inner.options.guard_interval;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(inner) = weak.upgrade() else { break };
                ModelManager { inner }.guard_once().await;
            }
        })
    }

    /// Frees memory for a model that needs `need`: within the budget, and
    /// (unless the user chose the maximum) never more than is free right
    /// now – unloading Ancilo's own idle models first.
    async fn make_room(&self, id: &str, need: u64, evict: bool) -> Result<()> {
        let settings = self.resource_settings();
        let budget = self.model_budget();
        loop {
            let used = self.used_bytes(Some(id)).await?;
            let state = self.system_state();
            let reclaimable = self.reclaimable(id).await;
            let fits_budget = used + need <= budget;
            let admitted = resources::admit(&settings, &state, need, reclaimable);
            if fits_budget && admitted.is_ok() {
                return Ok(());
            }
            // It ran like this a moment ago – the memory it gave back may
            // not show as free yet. Only an emergency keeps it away.
            if fits_budget
                && state.pressure != resources::Pressure::Critical
                && self.inner.restoring.lock().unwrap().contains(id)
            {
                tracing::info!(model = %id, why = %admitted.err().unwrap_or_default(), "back as it ran a moment ago");
                return Ok(());
            }
            if evict
                && (!fits_budget || reclaimable > 0)
                && let Some(victim) = self.eviction_candidate(id).await?
            {
                let instance = self.inner.instances.lock().await.remove(&victim);
                if let Some(i) = instance {
                    i.stop().await;
                }
                self.inner.speeds.lock().unwrap().remove(&victim);
                self.inner
                    .bus
                    .emit("model.evicted", Some(&victim), json!({"for": id}));
                self.wait_for_memory(id, need, Duration::from_secs(10))
                    .await;
                continue;
            }
            let loaded: Vec<String> = self
                .loaded()
                .await
                .into_iter()
                .map(|(m, _, _)| m.id)
                .filter(|m| m != id)
                .collect();
            let message = match admitted {
                Err(why) => ancilo_core::msg("model.no_room", &[("model", &id), ("why", &why)]),
                Ok(()) => ancilo_core::msg(
                    "model.over_budget",
                    &[
                        ("model", &id),
                        ("need", &mem(need)),
                        ("free", &mem(budget.saturating_sub(used))),
                        ("budget", &mem(budget)),
                    ],
                ),
            };
            return Err(Error::InsufficientResources(if loaded.is_empty() {
                message
            } else {
                ancilo_core::msg(
                    "model.others_loaded",
                    &[("message", &message), ("models", &loaded.join(", "))],
                )
            }));
        }
    }

    /// Memory Ancilo's idle, unpinned models would free.
    async fn reclaimable(&self, except: &str) -> u64 {
        self.loaded()
            .await
            .into_iter()
            .filter(|(m, _, _)| m.id != except && !m.busy && !m.working && !m.pinned)
            .map(|(_, ram, _)| ram)
            .sum()
    }

    pub fn hardware(&self) -> &HardwareProfile {
        &self.inner.hw
    }

    // ---- choosing a model ---------------------------------------------------

    /// The model catalog: the bundled one, or a newer one fetched earlier.
    /// `refresh`: fetch the newest from the Ancilo repository first – only
    /// when the user is choosing a model.
    pub async fn catalog(&self, refresh: bool) -> (Catalog, CatalogInfo) {
        let bundled = Catalog::bundled();
        let cache = self
            .inner
            .paths
            .home()
            .join("knowledge")
            .join("catalog.json");
        let newer = |c: Catalog| (c.updated > bundled.updated).then_some(c);
        let mut online = std::fs::read_to_string(&cache)
            .ok()
            .and_then(|t| Catalog::parse(&t).ok())
            .and_then(newer);
        let mut error = None;
        if refresh {
            match self.fetch_catalog().await {
                Ok((c, text)) => {
                    if let Some(c) = newer(c) {
                        if let Some(dir) = cache.parent() {
                            std::fs::create_dir_all(dir).ok();
                        }
                        std::fs::write(&cache, text).ok();
                        online = Some(c);
                    }
                }
                Err(e) => error = Some(e.message()),
            }
        }
        match online {
            Some(c) => {
                let info = CatalogInfo {
                    updated: c.updated.clone(),
                    source: "online".into(),
                    error,
                };
                (c, info)
            }
            None => {
                let info = CatalogInfo {
                    updated: bundled.updated.clone(),
                    source: "bundled".into(),
                    error,
                };
                (bundled, info)
            }
        }
    }

    async fn fetch_catalog(&self) -> Result<(Catalog, String)> {
        let url = &self.inner.config.catalog_url;
        let resp = self
            .inner
            .http
            .send(
                self.inner.http.get(url).timeout(Duration::from_secs(10)),
                Note::new(NetPurpose::Catalog, "recommended models", By::You),
            )
            .await
            .map_err(|e| Error::unavailable(format!("cannot fetch the model list: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::unavailable(format!(
                "the model list is not available (HTTP {})",
                resp.status().as_u16()
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::unavailable(format!("cannot fetch the model list: {e}")))?;
        if bytes.len() > 1 << 20 {
            return Err(Error::invalid("the model list is too large"));
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok((Catalog::parse(&text)?, text))
    }

    /// Which models suit this machine for these purposes – what fits next to
    /// the programs open right now, how fast it answers, what it costs to get.
    pub async fn recommend(&self, purposes: &[Purpose], refresh: bool) -> Result<Recommendations> {
        let (catalog, info) = self.catalog(refresh).await;
        let installed: Vec<Installed> = self
            .list()
            .await?
            .into_iter()
            .filter(|v| !v.cloud)
            .map(|v| Installed {
                tokens_per_sec: v.instance.as_ref().and_then(|i| i.tokens_per_sec),
                embedding: v.embedding,
                id: v.id,
            })
            .collect();
        // A hardware override (tests) describes a machine that is not this one.
        let available_now = if self.inner.config.hardware_override.is_some() {
            None
        } else {
            crate::hardware::available_memory_now()
        };
        let memory = catalog::memory_view(
            &self.inner.hw,
            self.model_budget(),
            available_now,
            self.used_bytes(None).await?,
        );
        let purposes = if purposes.is_empty() {
            vec![Purpose::Chat]
        } else {
            purposes.to_vec()
        };
        let (best, alternatives, more, too_big, embedding) = catalog::recommend(
            &catalog,
            &purposes,
            &self.inner.hw,
            memory.clone(),
            self.reserve(),
            &installed,
            self.resource_settings().variant,
        );
        Ok(Recommendations {
            purposes,
            best,
            alternatives,
            more,
            too_big,
            embedding,
            chip: self.inner.hw.chip.clone(),
            memory,
            catalog: info,
        })
    }

    /// GGUF models on Hugging Face matching `query` (network).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Err(Error::invalid("the search is empty"));
        }
        self.inner.hf.search(query, limit.clamp(1, 50)).await
    }

    pub fn bus(&self) -> &EventBus {
        &self.inner.bus
    }

    pub(crate) fn config(&self) -> &Config {
        &self.inner.config
    }

    pub(crate) fn db(&self) -> &Db {
        &self.inner.db
    }

    // ---- persistence ----------------------------------------------------

    fn save(&self, r: &ModelRecord) -> Result<()> {
        let source = serde_json::to_string(&r.source)?;
        let plan = serde_json::to_string(&r.plan)?;
        let meta = serde_json::to_string(r)?;
        let state = serde_json::to_value(r.state)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        let file = r.files.first().map(|p| p.display().to_string());
        self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO models(id, name, source, file_path, size_bytes, sha256, quant, state, plan, pinned, added_at, meta)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET name=excluded.name, source=excluded.source, file_path=excluded.file_path,
                   size_bytes=excluded.size_bytes, sha256=excluded.sha256, quant=excluded.quant, state=excluded.state,
                   plan=excluded.plan, meta=excluded.meta",
                params![r.id, r.name, source, file, r.size_bytes as i64, r.sha256, r.quant, state, plan, r.added_at.to_rfc3339(), meta],
            )
            .map(|_| ())
        })
    }

    fn records(&self) -> Result<Vec<ModelRecord>> {
        let rows: Vec<String> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT meta FROM models ORDER BY added_at, id")?;
            let rows = s.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })?;
        Ok(rows
            .iter()
            .filter_map(|m| serde_json::from_str(m).ok())
            .collect())
    }

    pub(crate) fn record(&self, id: &str) -> Result<Option<ModelRecord>> {
        let meta: Option<String> = self.inner.db.with(|c| {
            c.query_row("SELECT meta FROM models WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()
        })?;
        Ok(meta.and_then(|m| serde_json::from_str(&m).ok()))
    }

    fn roles_of(&self, id: &str) -> Result<Vec<String>> {
        self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT role FROM roles WHERE model_id = ?1 ORDER BY role")?;
            let rows = s.query_map(params![id], |r| r.get(0))?;
            rows.collect()
        })
    }

    pub(crate) fn role_holder(&self, role: &str) -> Result<Option<String>> {
        self.inner.db.with(|c| {
            c.query_row(
                "SELECT model_id FROM roles WHERE role = ?1",
                params![role],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn assign_role(&self, role: &str, model_id: &str) -> Result<()> {
        if self.record(model_id)?.is_none() {
            return Err(Error::not_found(format!("no model '{model_id}'")));
        }
        self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO roles(role, model_id) VALUES(?1, ?2) ON CONFLICT(role) DO UPDATE SET model_id = excluded.model_id",
                params![role, model_id],
            )
            .map(|_| ())
        })?;
        self.inner
            .bus
            .emit("role.assigned", Some(model_id), json!({"role": role}));
        Ok(())
    }

    /// Finds a model by id, unique id prefix or (case-insensitive) name.
    pub fn find(&self, reference: &str) -> Result<ModelRecord> {
        let all = self.records()?;
        let r = reference.trim().to_ascii_lowercase();
        if let Some(m) = all.iter().find(|m| m.id == r) {
            return Ok(m.clone());
        }
        let matches: Vec<&ModelRecord> = all
            .iter()
            .filter(|m| m.id.starts_with(&r) || m.name.to_ascii_lowercase() == r)
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(Error::not_found(format!(
                "no model '{reference}' – see `ancilo list`"
            ))),
            many => Err(Error::invalid(format!(
                "'{reference}' is ambiguous: {}",
                many.iter()
                    .map(|m| m.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    // ---- views ----------------------------------------------------------

    async fn view(&self, r: &ModelRecord) -> Result<ModelView> {
        let instance = self.inner.instances.lock().await.get(&r.id).cloned();
        let download = self
            .inner
            .downloads
            .lock()
            .unwrap()
            .get(&r.id)
            .map(|d| d.1.clone());
        let status = match (r.state, &instance) {
            _ if matches!(r.source, ModelSource::Remote { .. }) => ModelStatus::Running,
            (FileState::Downloading, _) => ModelStatus::Downloading,
            (FileState::Failed, _) => ModelStatus::DownloadFailed,
            (FileState::Ready, None) => ModelStatus::Ready,
            (FileState::Ready, Some(i)) => match i.info().state {
                InstanceState::Starting => ModelStatus::Starting,
                InstanceState::Running => ModelStatus::Running,
                InstanceState::Crashed => ModelStatus::Crashed,
                InstanceState::Failed => ModelStatus::Failed,
                InstanceState::Stopped => ModelStatus::Ready,
            },
        };
        let source = match (&r.source, r.found_in) {
            (ModelSource::HuggingFace { repo, .. }, Some(f)) => {
                format!("Hugging Face · {repo} (existing file from {})", f.label())
            }
            (ModelSource::HuggingFace { repo, .. }, None) => format!("Hugging Face · {repo}"),
            (ModelSource::Local, _) => "local file".into(),
            (
                ModelSource::Remote {
                    base_url,
                    model,
                    cloud,
                },
                _,
            ) => format!(
                "{} · {model} at {base_url}",
                if *cloud { "cloud" } else { "server" }
            ),
        };
        let tps = self.inner.speeds.lock().unwrap().get(&r.id).copied();
        Ok(ModelView {
            id: r.id.clone(),
            name: r.name.clone(),
            quant: r.quant.clone(),
            size_bytes: r.size_bytes,
            source,
            path: r.files.first().cloned(),
            status,
            failure: r
                .failure
                .clone()
                .or_else(|| instance.as_ref().and_then(|i| i.info().last_error)),
            roles: self.roles_of(&r.id)?,
            pinned: r.pinned,
            cloud: matches!(r.source, ModelSource::Remote { cloud: true, .. }),
            embedding: r.embedding,
            ctx_tokens: r.plan.ctx_tokens,
            expected_ram_bytes: r.plan.expected_ram_bytes,
            fit: r.plan.fit,
            download,
            instance: instance.map(|i| {
                let info = i.info();
                InstanceView {
                    port: info.port,
                    load_ms: info.load_ms,
                    restarts: info.restarts,
                    tokens_per_sec: tps,
                    last_error: info.last_error,
                }
            }),
        })
    }

    pub async fn list(&self) -> Result<Vec<ModelView>> {
        let mut out = Vec::new();
        for r in self.records()? {
            out.push(self.view(&r).await?);
        }
        Ok(out)
    }

    pub async fn status(&self, reference: &str) -> Result<ModelView> {
        let r = self.find(reference)?;
        self.view(&r).await
    }

    // ---- memory ---------------------------------------------------------

    fn model_budget(&self) -> u64 {
        let hw = &self.inner.hw;
        let ram = hw.total_ram_bytes.saturating_sub(self.reserve());
        match (hw.gpu, hw.gpu_memory_bytes) {
            (Gpu::Metal | Gpu::Cuda, Some(g)) => g.min(ram),
            _ => ram,
        }
    }

    async fn used_bytes(&self, except: Option<&str>) -> Result<u64> {
        let ids: Vec<String> = self
            .inner
            .instances
            .lock()
            .await
            .iter()
            .filter(|(id, i)| {
                Some(id.as_str()) != except
                    && !matches!(
                        i.info().state,
                        InstanceState::Stopped | InstanceState::Failed
                    )
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut used = 0;
        for id in ids {
            if let Some(r) = self.record(&id)? {
                used += r.plan.expected_ram_bytes;
            }
        }
        Ok(used)
    }

    pub async fn hardware_view(&self) -> Result<HardwareView> {
        let budget = self.model_budget();
        let used = self.used_bytes(None).await?;
        Ok(HardwareView {
            hardware: self.inner.hw.clone(),
            model_budget_bytes: budget,
            used_bytes: used,
            free_for_models_bytes: budget.saturating_sub(used),
        })
    }

    /// Memory kept for the system and other programs: the configured
    /// reserve, or more when the resource level allows Ancilo less.
    fn reserve(&self) -> u64 {
        let total = self.inner.hw.total_ram_bytes;
        let share = self.inner.resources.lock().unwrap().max_share;
        let by_level = total - (total as f64 * share) as u64;
        self.inner.config.ram_reserve_bytes(total).max(by_level)
    }

    fn search_dirs(&self) -> Vec<PathBuf> {
        self.inner
            .config
            .model_search_dirs
            .clone()
            .unwrap_or_else(discovery::default_dirs)
    }

    // ---- planning & adding ----------------------------------------------

    /// What `add` would do, without doing it.
    ///
    /// The verdict answers "does this model fit this machine?" – independent of
    /// what is loaded right now. Whether there is room *at the moment* is
    /// checked when a model is started.
    pub async fn plan(&self, address: &str, wish: &Wish) -> Result<PlanPreview> {
        let addr = address::resolve(address)?;
        let used = 0;
        match &addr {
            Address::ServerUrl { url } => Err(Error::invalid(format!(
                "{url} is a server – there is nothing to plan; add it with `ancilo add {url}` (it needs no memory here)"
            ))),
            Address::LocalFile { path } => {
                let (meta, size) = self.local_meta(path)?;
                let file = RepoFile {
                    path: path.display().to_string(),
                    size,
                    sha256: None,
                };
                let plan = planner::plan(
                    &self.inner.hw,
                    &[file],
                    Some(&ModelShape::from(&meta)),
                    wish,
                    used,
                    self.reserve(),
                )?;
                let existing = Some(path.clone());
                Ok(PlanPreview {
                    address: addr.clone(),
                    repo: None,
                    plan,
                    existing,
                    existing_in: None,
                    download_bytes: 0,
                })
            }
            Address::HuggingFace {
                repo,
                file,
                quant,
                revision,
            } => {
                let (info, files) = tokio::try_join!(
                    self.inner.hf.model_info(repo),
                    self.inner.hf.files(repo, revision)
                )?;
                let wish = Wish {
                    context: wish.context,
                    quant: wish.quant.clone().or_else(|| quant.clone()),
                    file: wish.file.clone().or_else(|| file.clone()),
                    remote_model: None,
                };
                let plan = planner::plan(
                    &self.inner.hw,
                    &files,
                    Some(&info.shape()),
                    &wish,
                    used,
                    self.reserve(),
                )?;
                let found = discovery::scan(&self.search_dirs());
                let existing: Vec<_> = plan
                    .files
                    .iter()
                    .map(|f| discovery::find(&found, &f.path, f.size, f.sha256.as_deref()).cloned())
                    .collect();
                let complete = existing.iter().all(Option::is_some);
                let download_bytes = if complete { 0 } else { plan.size_bytes };
                Ok(PlanPreview {
                    address: addr.clone(),
                    repo: Some(info),
                    existing: complete
                        .then(|| existing[0].as_ref().map(|f| f.path.clone()))
                        .flatten(),
                    existing_in: complete
                        .then(|| existing[0].as_ref().map(|f| f.found_in))
                        .flatten(),
                    plan,
                    download_bytes,
                })
            }
        }
    }

    fn local_meta(&self, path: &Path) -> Result<(GgufMeta, u64)> {
        let size = std::fs::metadata(path)
            .map_err(|_| Error::not_found(format!("file not found: {}", path.display())))?
            .len();
        Ok((gguf::read_meta_file(path)?, size))
    }

    fn unique_id(&self, base: &str) -> Result<String> {
        let base = if base.is_empty() {
            "model".to_string()
        } else {
            base.to_string()
        };
        let ids: Vec<String> = self.records()?.into_iter().map(|r| r.id).collect();
        if !ids.contains(&base) {
            return Ok(base);
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|c| !ids.contains(c))
            .ok_or_else(|| Error::internal("no id"))
    }

    /// Adds a model to the library: uses an existing copy or downloads it, then
    /// (optionally) starts it. Returns immediately; progress arrives as events.
    pub async fn add(&self, address: &str, wish: &Wish, start: bool) -> Result<ModelView> {
        if let Address::ServerUrl { url } = address::resolve(address)? {
            return self
                .add_remote(&url, wish.remote_model.as_deref(), None, false)
                .await;
        }
        let preview = self.plan(address, wish).await?;
        if preview.plan.fit == Fit::DoesNotFit {
            return Err(Error::InsufficientResources(ancilo_core::msg(
                "model.does_not_fit",
                &[("reason", &preview.plan.reason)],
            )));
        }
        let (source, name) = match &preview.address {
            Address::HuggingFace { repo, revision, .. } => (
                ModelSource::HuggingFace {
                    repo: repo.clone(),
                    revision: revision.clone(),
                },
                repo.rsplit('/')
                    .next()
                    .unwrap_or(repo)
                    .trim_end_matches("-GGUF")
                    .trim_end_matches("-gguf")
                    .to_string(),
            ),
            _ => (ModelSource::Local, String::new()),
        };
        let first = preview.plan.primary_file().path.clone();
        let stem = Path::new(&first)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = stem.split("-00001-of-").next().unwrap_or(&stem).to_string();
        let name = if name.is_empty() { stem.clone() } else { name };

        // Idempotent: the same file is only added once.
        for r in self.records()? {
            let same_source = r.source == source
                && r.plan.files.iter().map(|f| &f.path).eq(preview
                    .plan
                    .files
                    .iter()
                    .map(|f| &f.path));
            let same_local = matches!(source, ModelSource::Local)
                && r.files.first().map(|p| p.display().to_string()) == Some(first.clone());
            if same_source || same_local {
                if start && r.state == FileState::Ready {
                    return self.start(&r.id, None).await;
                }
                return self.view(&r).await;
            }
        }

        let id = self.unique_id(&slug(&stem))?;
        let downloading = preview.existing.is_none();
        let files: Vec<PathBuf> = if let Some(existing) = &preview.existing {
            if preview.plan.files.len() == 1 {
                vec![existing.clone()]
            } else {
                // Split models from other tools: use each found part.
                let found = discovery::scan(&self.search_dirs());
                preview
                    .plan
                    .files
                    .iter()
                    .filter_map(|f| {
                        discovery::find(&found, &f.path, f.size, f.sha256.as_deref())
                            .map(|x| x.path.clone())
                    })
                    .collect()
            }
        } else {
            let repo = match &source {
                ModelSource::HuggingFace { repo, .. } => repo.clone(),
                ModelSource::Local | ModelSource::Remote { .. } => String::new(),
            };
            preview
                .plan
                .files
                .iter()
                .map(|f| self.inner.paths.models_dir().join(&repo).join(&f.path))
                .collect()
        };
        if downloading {
            let free = self.inner.hw.free_disk_bytes;
            if free > 0 && preview.download_bytes + 1_000_000_000 > free {
                return Err(Error::InsufficientResources(ancilo_core::msg(
                    "download.no_space",
                    &[
                        ("need", &disk(preview.download_bytes)),
                        ("free", &disk(free)),
                    ],
                )));
            }
        }
        let meta = if downloading {
            None
        } else {
            gguf::read_meta_file(&files[0]).ok()
        };
        let hf_says_embedding = preview
            .repo
            .as_ref()
            .and_then(|r| r.pipeline_tag.as_deref())
            .is_some_and(|t| matches!(t, "feature-extraction" | "sentence-similarity"));
        let repo_name = preview
            .repo
            .as_ref()
            .map(|r| r.id.as_str())
            .unwrap_or_default();
        let embedding = hf_says_embedding
            || looks_like_embedding(&format!("{repo_name} {stem}"), meta.as_ref());
        let record = ModelRecord {
            id: id.clone(),
            name,
            source,
            files,
            size_bytes: preview.plan.size_bytes,
            sha256: preview.plan.primary_file().sha256.clone(),
            quant: preview.plan.quant.clone(),
            state: if downloading {
                FileState::Downloading
            } else {
                FileState::Ready
            },
            failure: None,
            plan: preview.plan.clone(),
            meta,
            embedding,
            found_in: preview.existing_in,
            autostart: false,
            pinned: embedding,
            added_at: Utc::now(),
        };
        self.save(&record)?;
        let role = if embedding { ROLE_EMBED } else { ROLE_DEFAULT };
        if self.role_holder(role)?.is_none() {
            self.assign_role(role, &id)?;
        }
        self.inner.bus.emit(
            "model.added",
            Some(&id),
            json!({"name": record.name, "quant": record.quant, "downloading": downloading, "existing": preview.existing}),
        );
        if downloading {
            self.spawn_download(record.clone(), start, By::You);
        } else if start {
            return self.start(&id, None).await;
        }
        self.view(&record).await
    }

    fn spawn_download(&self, record: ModelRecord, start: bool, by: By) {
        let cancel = CancellationToken::new();
        // A resumed download shows its real progress right away (the partial
        // file is re-hashed before new progress events arrive).
        let resumed: u64 = record
            .files
            .iter()
            .map(|f| {
                std::fs::metadata(format!("{}.part", f.display()))
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum();
        let percent =
            (record.size_bytes > 0).then(|| resumed as f64 / record.size_bytes as f64 * 100.0);
        self.inner.downloads.lock().unwrap().insert(
            record.id.clone(),
            (
                cancel.clone(),
                DownloadView {
                    bytes: resumed,
                    total: Some(record.size_bytes),
                    percent,
                    bytes_per_sec: None,
                },
            ),
        );
        let me = self.clone();
        tokio::spawn(async move {
            let id = record.id.clone();
            let result = me.run_download(&record, &cancel, by).await;
            me.inner.downloads.lock().unwrap().remove(&id);
            match result {
                Ok(()) => {
                    if start && let Err(e) = me.start(&id, None).await {
                        tracing::warn!(model = %id, error = %e, "start after download failed");
                    }
                }
                Err(e) => {
                    if let Ok(Some(mut r)) = me.record(&id) {
                        r.state = FileState::Failed;
                        r.failure = Some(e.message());
                        let _ = me.save(&r);
                    }
                    me.inner
                        .bus
                        .emit("model.failed", Some(&id), json!({"reason": e.message()}));
                }
            }
        });
    }

    async fn run_download(
        &self,
        record: &ModelRecord,
        cancel: &CancellationToken,
        by: By,
    ) -> Result<()> {
        let ModelSource::HuggingFace { repo, revision } = &record.source else {
            return Err(Error::internal("only Hugging Face models are downloaded"));
        };
        // Mirror progress into the view.
        let mut rx = self.inner.bus.subscribe();
        let me = self.clone();
        let id = record.id.clone();
        let done_before: u64 = 0;
        let watcher = tokio::spawn(async move {
            while let Ok(e) = rx.recv().await {
                if e.subject.as_deref() != Some(id.as_str()) {
                    continue;
                }
                let mut d = me.inner.downloads.lock().unwrap();
                let Some(entry) = d.get_mut(&id) else {
                    continue;
                };
                match e.kind.as_str() {
                    // A resumed download starts where it stopped, not at 0 %.
                    "download.started" => {
                        let bytes = e.data["resumed_at"].as_u64().unwrap_or(0);
                        let total = e.data["total"].as_u64();
                        entry.1 = DownloadView {
                            bytes,
                            total,
                            percent: total
                                .filter(|t| *t > 0)
                                .map(|t| bytes as f64 / t as f64 * 100.0),
                            bytes_per_sec: None,
                        };
                    }
                    "download.progress" => {
                        entry.1 = DownloadView {
                            bytes: done_before + e.data["bytes"].as_u64().unwrap_or(0),
                            total: e.data["total"].as_u64(),
                            percent: e.data["percent"].as_f64(),
                            bytes_per_sec: e.data["bytes_per_sec"].as_f64(),
                        };
                    }
                    _ => {}
                }
            }
        });
        let mut result = Ok(());
        for (file, dest) in record.plan.files.iter().zip(&record.files) {
            let spec = DownloadSpec {
                url: self.inner.hf.file_url(repo, revision, &file.path),
                dest: dest.clone(),
                size: Some(file.size),
                sha256: file.sha256.clone(),
                bearer: crate::hf::token(),
                note: Note::new(
                    NetPurpose::ModelDownload,
                    format!("{repo}/{}", file.path),
                    by,
                ),
            };
            result = download::download(
                &self.inner.http,
                &spec,
                &self.inner.bus,
                &record.id,
                &self.inner.options.download,
                cancel,
            )
            .await;
            if result.is_err() {
                break;
            }
        }
        watcher.abort();
        result?;
        let mut r = self
            .record(&record.id)?
            .ok_or_else(|| Error::not_found("model removed during download"))?;
        r.meta = gguf::read_meta_file(&r.files[0]).ok();
        if let Some(meta) = &r.meta {
            r.embedding = r.embedding || looks_like_embedding(&r.id, Some(meta));
            if let Some(max) = meta.context_length {
                r.plan.ctx_tokens = r.plan.ctx_tokens.min(max);
            }
        }
        r.state = FileState::Ready;
        r.failure = None;
        self.save(&r)?;
        self.inner
            .bus
            .emit("model.downloaded", Some(&r.id), json!({"files": r.files}));
        Ok(())
    }

    // ---- running --------------------------------------------------------

    /// Installs the pinned llama.cpp build explicitly (`ancilo llama install`).
    pub async fn install_llama(&self) -> Result<serde_json::Value> {
        let build = self.inner.llama_build.clone().ok_or_else(|| {
            Error::Unavailable(format!(
                "no llama.cpp build for {}/{} – set llama_server_bin in config.toml",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
        let tag = build.tag;
        let path = llama::install(
            &self.inner.paths,
            &self.inner.config,
            &self.inner.http,
            &self.inner.bus,
            build,
            By::You,
        )
        .await?;
        *self.inner.binary.lock().await = None;
        Ok(json!({"tag": tag, "path": path}))
    }

    async fn binary(&self) -> Result<PathBuf> {
        let mut guard = self.inner.binary.lock().await;
        if let Some(b) = guard.as_ref() {
            return Ok(b.clone());
        }
        let b = llama::ensure_binary(
            &self.inner.paths,
            &self.inner.config,
            &self.inner.http,
            &self.inner.bus,
            self.inner.llama_build.clone(),
        )
        .await?;
        *guard = Some(b.clone());
        Ok(b)
    }

    /// Loads a model into memory. Returns once the process is starting; the
    /// `model.running` event (with tokens/s) follows when it is ready.
    /// Starts a model the user asked for: it never unloads other models to
    /// make room (that only happens when a model is loaded on demand).
    pub async fn start(&self, reference: &str, context: Option<ContextSize>) -> Result<ModelView> {
        self.start_with(reference, context, false).await
    }

    async fn start_with(
        &self,
        reference: &str,
        context: Option<ContextSize>,
        evict: bool,
    ) -> Result<ModelView> {
        let mut r = self.find(reference)?;
        if matches!(r.source, ModelSource::Remote { .. }) {
            // Nothing to start: the server runs elsewhere.
            return self.status(&r.id).await;
        }
        match r.state {
            FileState::Downloading => {
                return Err(Error::Conflict(format!("'{}' is still downloading", r.id)));
            }
            FileState::Failed => {
                return Err(Error::Conflict(format!(
                    "'{}' is not available: {}",
                    r.id,
                    r.failure.clone().unwrap_or_default()
                )));
            }
            FileState::Ready => {}
        }
        // Bind first: a lock guard in an `if let` condition would live through
        // the body and deadlock with `view`.
        let existing = self.inner.instances.lock().await.get(&r.id).cloned();
        if let Some(existing) = existing
            && matches!(
                existing.info().state,
                InstanceState::Starting | InstanceState::Running | InstanceState::Crashed
            )
        {
            return self.view(&r).await;
        }
        if let Some(ctx) = context
            && ctx.tokens() != r.plan.ctx_tokens
        {
            let shape = r.meta.as_ref().map(ModelShape::from);
            let files: Vec<RepoFile> = r.plan.files.clone();
            let wish = Wish {
                context: ctx,
                file: Some(files[0].path.clone()),
                quant: None,
                remote_model: None,
            };
            r.plan = planner::plan(
                &self.inner.hw,
                &files,
                shape.as_ref(),
                &wish,
                0,
                self.reserve(),
            )?;
            self.save(&r)?;
        }
        self.make_room(&r.id, r.plan.expected_ram_bytes, evict)
            .await?;
        let settings = self.resource_settings();
        let binary = self.binary().await?;
        let run_dir = self.inner.paths.home().join("run");
        std::fs::create_dir_all(&run_dir)?;
        let spec = LaunchSpec {
            binary,
            model_path: r.files[0].clone(),
            alias: r.id.clone(),
            ctx_tokens: r.plan.ctx_tokens,
            gpu_layers: r.plan.gpu_layers,
            threads: r.plan.threads,
            embeddings: r.embedding,
            log_file: self.inner.paths.logs_dir().join(format!("{}.log", r.id)),
            api_key: uuid::Uuid::new_v4().simple().to_string(),
            api_key_file: run_dir.join(format!("{}.key", r.id)),
            load_timeout: Duration::from_secs(120 + r.size_bytes / 1_000_000_000 * 10),
            env: self
                .inner
                .config
                .llama_server_env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            max_restarts: 3,
            restart_window: Duration::from_secs(600),
            restart_backoff: self.inner.options.restart_backoff,
            parallel: if r.embedding { 1 } else { settings.parallel },
            priority: settings.priority,
        };
        let instance = Arc::new(Instance::spawn(spec, self.inner.bus.clone(), r.id.clone())?);
        self.inner
            .instances
            .lock()
            .await
            .insert(r.id.clone(), instance.clone());
        r.autostart = true;
        self.save(&r)?;
        let me = self.clone();
        let id = r.id.clone();
        let embedding = r.embedding;
        let timeout = Duration::from_secs(150 + r.size_bytes / 1_000_000_000 * 10);
        tokio::spawn(async move {
            if instance.wait_ready(timeout).await.is_ok() {
                let tps = if me.inner.options.measure_speed && !embedding {
                    me.measure_speed(&instance, &id).await
                } else {
                    None
                };
                if let Some(t) = tps {
                    me.inner.speeds.lock().unwrap().insert(id.clone(), t);
                }
                me.inner.bus.emit(
                    "model.running",
                    Some(&id),
                    json!({"tokens_per_sec": tps, "load_ms": instance.info().load_ms}),
                );
            }
        });
        self.view(&r).await
    }

    /// Short generation to measure tokens per second.
    async fn measure_speed(&self, instance: &Instance, id: &str) -> Option<f64> {
        let url = format!("{}/v1/chat/completions", instance.base_url()?);
        let started = Instant::now();
        // To the local model: stays on this computer (not logged).
        let resp: Value = self
            .inner
            .http
            .send(
                self.inner
                    .http
                    .post(url)
                    .bearer_auth(&instance.api_key)
                    .timeout(Duration::from_secs(60))
                    .json(&json!({
                        "model": id,
                        "messages": [{"role": "user", "content": "Count from 1 to 40, separated by spaces."}],
                        "max_tokens": 64,
                        "temperature": 0,
                    })),
                Note::new(NetPurpose::CloudModel, id, By::Ancilo),
            )
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        resp["timings"]["predicted_per_second"]
            .as_f64()
            .or_else(|| {
                let tokens = resp["usage"]["completion_tokens"].as_f64()?;
                let secs = started.elapsed().as_secs_f64();
                (secs > 0.0).then(|| tokens / secs)
            })
    }

    pub async fn stop(&self, reference: &str) -> Result<ModelView> {
        let mut r = self.find(reference)?;
        let instance = self.inner.instances.lock().await.remove(&r.id);
        if let Some(i) = instance {
            i.stop().await;
        }
        self.inner.speeds.lock().unwrap().remove(&r.id);
        r.autostart = false;
        self.save(&r)?;
        self.view(&r).await
    }

    /// Removes a model from the library. Files downloaded by Ancilo are deleted
    /// (unless `keep_files`); files of other tools are never touched.
    pub async fn remove(&self, reference: &str, keep_files: bool) -> Result<Vec<PathBuf>> {
        let r = self.find(reference)?;
        if matches!(r.source, ModelSource::Remote { .. }) {
            self.inner.options.secrets.delete(&secret_name(&r.id))?;
        }
        let download = self.inner.downloads.lock().unwrap().remove(&r.id);
        if let Some((cancel, _)) = download {
            cancel.cancel();
        }
        let instance = self.inner.instances.lock().await.remove(&r.id);
        if let Some(i) = instance {
            i.stop().await;
        }
        let mut deleted = Vec::new();
        let models_dir = self.inner.paths.models_dir();
        if !keep_files && r.found_in.is_none() && !matches!(r.source, ModelSource::Local) {
            for f in &r.files {
                if f.starts_with(&models_dir) {
                    for p in [f.clone(), PathBuf::from(format!("{}.part", f.display()))] {
                        if tokio::fs::remove_file(&p).await.is_ok() {
                            deleted.push(p);
                        }
                    }
                }
            }
        }
        self.inner.db.with(|c| {
            c.execute("DELETE FROM models WHERE id = ?1", params![r.id])
                .map(|_| ())
        })?;
        self.inner
            .bus
            .emit("model.removed", Some(&r.id), json!({"deleted": deleted}));
        Ok(deleted)
    }

    /// Restarts a model with a larger context so that a request of
    /// `needed_tokens` fits (next power of two with headroom, capped by what
    /// the model supports). Returns `false` if the context cannot grow.
    ///
    /// A model that works keeps working: it is only restarted when the larger
    /// context fits now (counting the memory it frees itself), and if the
    /// restart fails anyway it comes back with its old context.
    pub async fn grow_context(
        &self,
        id: &str,
        needed_tokens: u64,
        timeout: Duration,
    ) -> Result<bool> {
        let mut r = self.find(id)?;
        let model_max = r
            .meta
            .as_ref()
            .and_then(|m| m.context_length)
            .unwrap_or(u64::MAX);
        let target = (needed_tokens.saturating_mul(5) / 4)
            .next_power_of_two()
            .max(r.plan.ctx_tokens.saturating_mul(2))
            .min(model_max);
        if target <= r.plan.ctx_tokens {
            return Ok(false);
        }
        let shape = r.meta.as_ref().map(ModelShape::from);
        let wish = Wish {
            context: ContextSize::Large,
            quant: None,
            file: Some(r.plan.files[0].path.clone()),
            remote_model: None,
        };
        let plan = planner::plan_with_ctx(
            &self.inner.hw,
            &r.plan.files,
            shape.as_ref(),
            &wish,
            target,
            0,
            self.reserve(),
        )?;
        if plan.fit == Fit::DoesNotFit {
            return Err(Error::InsufficientResources(format!(
                "'{}' would need a context of {} tokens for this request, which does not fit in memory: {}",
                r.id, target, plan.reason
            )));
        }
        let from = r.plan.ctx_tokens;
        let old_plan = r.plan.clone();
        let need = plan.expected_ram_bytes;
        // Fits now? A fresh look, crediting what the running model frees.
        let running = self.endpoint(&r.id).await.is_some();
        let own = if running {
            old_plan.expected_ram_bytes
        } else {
            0
        };
        self.forget_measurement();
        let state = self.system_state();
        let reclaimable = self.reclaimable(&r.id).await + own;
        let used = self.used_bytes(Some(&r.id)).await?;
        let room = resources::admit(&self.resource_settings(), &state, need, reclaimable);
        if room.is_err() || used + need > self.model_budget() {
            tracing::info!(
                model = %r.id,
                from,
                to = plan.ctx_tokens,
                why = %room.err().unwrap_or_else(|| "over the budget".into()),
                "the context cannot grow now – the model keeps running as it is"
            );
            return Ok(false);
        }
        let instance = self.inner.instances.lock().await.remove(&r.id);
        if let Some(i) = instance {
            i.stop().await;
        }
        r.plan = plan;
        self.save(&r)?;
        // The memory comes back with a delay (the OS reports it late).
        self.wait_for_memory(&r.id, need, Duration::from_secs(15))
            .await;
        if let Err(e) = self.ensure_running(&r.id, timeout).await {
            tracing::warn!(model = %r.id, error = %e.message(), "the larger context did not start – back to the old one");
            r.plan = old_plan;
            self.save(&r)?;
            self.wait_for_memory(&r.id, r.plan.expected_ram_bytes, Duration::from_secs(15))
                .await;
            if running {
                self.inner.restoring.lock().unwrap().insert(r.id.clone());
                let back = self.ensure_running(&r.id, timeout).await;
                self.inner.restoring.lock().unwrap().remove(&r.id);
                back?;
            }
            return Ok(false);
        }
        self.inner.bus.emit(
            "model.context_grown",
            Some(&r.id),
            json!({"from": from, "to": r.plan.ctx_tokens}),
        );
        Ok(true)
    }

    /// The next look at the system measures again (after a model stopped).
    fn forget_measurement(&self) {
        self.inner.measured.lock().unwrap().take();
    }

    /// Waits – at most `max` – until the system reports the memory a model
    /// needs as free: memory of a stopped model comes back with a delay.
    async fn wait_for_memory(&self, id: &str, need: u64, max: Duration) {
        // Test probes do not change by themselves.
        if self.inner.config.system_probe_override.is_some() {
            return;
        }
        let settings = self.resource_settings();
        let until = Instant::now() + max;
        loop {
            self.forget_measurement();
            let state = self.system_state();
            let reclaimable = self.reclaimable(id).await;
            if resources::admit(&settings, &state, need, reclaimable).is_ok()
                || Instant::now() >= until
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Marks a model as the one an agent works with, for the whole turn –
    /// also while its tools run between requests. It is not unloaded for
    /// being idle or because memory got tight, and not evicted for another
    /// model; only an emergency (critical memory or heat) unloads it.
    pub fn begin_work(&self, id: &str) -> WorkGuard {
        *self
            .inner
            .working
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default() += 1;
        WorkGuard {
            inner: self.inner.clone(),
            id: id.to_string(),
        }
    }

    /// Marks a model as in use: protects it from eviction and records the
    /// time of use when the guard is dropped.
    pub fn begin_use(&self, id: &str) -> UseGuard {
        *self
            .inner
            .busy
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default() += 1;
        self.inner
            .last_used
            .lock()
            .unwrap()
            .insert(id.to_string(), Instant::now());
        UseGuard {
            inner: self.inner.clone(),
            id: id.to_string(),
        }
    }

    /// Makes sure a model is running and returns its endpoint. Loads it on
    /// demand; if memory is short, unloads the least recently used idle,
    /// unpinned models first. Never exceeds the memory budget.
    pub async fn ensure_running(&self, id: &str, timeout: Duration) -> Result<Endpoint> {
        if let Some(ep) = self.endpoint(id).await {
            return Ok(ep);
        }
        {
            let _loading = self.inner.loading.lock().await;
            let r = self.find(id)?;
            let existing = self.inner.instances.lock().await.get(&r.id).cloned();
            let starting = existing.is_some_and(|i| {
                matches!(
                    i.info().state,
                    InstanceState::Starting | InstanceState::Running | InstanceState::Crashed
                )
            });
            if !starting {
                // Makes room first (budget, memory free right now) – by
                // unloading idle models if needed.
                self.start_with(&r.id, None, true).await?;
            }
        }
        let instance = self
            .inner
            .instances
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::unavailable(format!("'{id}' was stopped while loading")))?;
        instance.wait_ready(timeout).await?;
        self.endpoint(id)
            .await
            .ok_or_else(|| Error::unavailable(format!("'{id}' is not running")))
    }

    /// Least recently used running model that is neither busy nor pinned.
    async fn eviction_candidate(&self, except: &str) -> Result<Option<String>> {
        let running: Vec<String> = self
            .inner
            .instances
            .lock()
            .await
            .iter()
            .filter(|(id, i)| {
                id.as_str() != except
                    && !matches!(
                        i.info().state,
                        InstanceState::Stopped | InstanceState::Failed
                    )
            })
            .map(|(id, _)| id.clone())
            .collect();
        let busy = self.inner.busy.lock().unwrap().clone();
        let working = self.inner.working.lock().unwrap().clone();
        let last = self.inner.last_used.lock().unwrap().clone();
        let mut candidates = Vec::new();
        for id in running {
            // Not the model of an agent at work, either: its next step needs it.
            if busy.get(&id).copied().unwrap_or(0) > 0 || working.get(&id).copied().unwrap_or(0) > 0
            {
                continue;
            }
            if self.record(&id)?.is_some_and(|r| r.pinned) {
                continue;
            }
            candidates.push((last.get(&id).copied(), id));
        }
        // Never used (None) sorts first, then oldest use.
        candidates.sort();
        Ok(candidates.into_iter().next().map(|(_, id)| id))
    }

    pub fn set_pinned(&self, reference: &str, pinned: bool) -> Result<()> {
        let mut r = self.find(reference)?;
        r.pinned = pinned;
        self.save(&r)
    }

    /// Model id for a name used by a client: a model id, a role, or anything
    /// else (e.g. `claude-sonnet-…`) → the default model.
    pub fn resolve_name(&self, name: &str) -> Result<String> {
        Ok(self.route(&crate::routing::RouteRequest::name(name))?.model)
    }

    /// Resumes a download that failed (the partial file is kept).
    pub async fn retry_download(&self, reference: &str) -> Result<ModelView> {
        let mut r = self.find(reference)?;
        if r.state != FileState::Failed {
            return Err(Error::Conflict(format!(
                "'{}' has no failed download",
                r.id
            )));
        }
        r.state = FileState::Downloading;
        r.failure = None;
        self.save(&r)?;
        self.inner
            .bus
            .emit("download.retrying", Some(&r.id), json!({"manual": true}));
        self.spawn_download(r.clone(), false, By::You);
        self.status(&r.id).await
    }

    /// The Hugging Face model card (README) of a model, if it has one.
    pub async fn model_card(&self, reference: &str) -> Result<Option<String>> {
        let r = self.find(reference)?;
        let ModelSource::HuggingFace { repo, revision } = &r.source else {
            return Ok(None);
        };
        self.inner.hf.text_file(repo, revision, "README.md").await
    }

    /// The model serving the `embed` role, if it is an embedding model.
    /// Never a cloud model: embeddings are made from code and documents.
    pub fn embedding_model(&self) -> Option<String> {
        let id = self.role_holder(ROLE_EMBED).ok().flatten()?;
        self.record(&id)
            .ok()
            .flatten()
            .filter(|r| r.embedding && !self.is_cloud(&r.id))
            .map(|r| r.id)
    }

    /// A model id for a model reference, role or alias – an error for
    /// anything else (unlike `resolve_name`, which falls back to the default).
    pub fn resolve_strict(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if let Ok(r) = self.find(name) {
            return Ok(r.id);
        }
        if let Some(id) = self.role_holder(name)? {
            return Ok(id);
        }
        if let Some(target) = self.inner.config.model_aliases.get(name) {
            return self.resolve_strict(target);
        }
        Err(Error::not_found(format!("no model or role '{name}'")))
    }

    /// `resolve_name` without routing rules (used by the router itself).
    pub(crate) fn resolve_plain(&self, name: &str) -> Result<String> {
        let name = name.trim();
        if !name.is_empty() {
            if let Ok(r) = self.find(name) {
                return Ok(r.id);
            }
            if let Some(id) = self.role_holder(name)? {
                return Ok(id);
            }
            if let Some(target) = self.inner.config.model_aliases.get(name) {
                return self.resolve_plain(target);
            }
        }
        self.role_holder(ROLE_DEFAULT)?.ok_or_else(|| {
            Error::not_found("no default model yet – add one with `ancilo add <address>`")
        })
    }

    /// All models and roles, for `/v1/models`.
    pub fn names(&self) -> Result<ModelNames> {
        let roles: Vec<(String, String)> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT role, model_id FROM roles ORDER BY role")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        Ok(ModelNames {
            models: self.records()?,
            roles,
        })
    }

    /// Base URL and API key of a running model (for the gateway).
    pub async fn endpoint(&self, id: &str) -> Option<Endpoint> {
        if let Ok(Some(r)) = self.record(id)
            && let ModelSource::Remote {
                base_url,
                model,
                cloud,
            } = &r.source
        {
            let key = self
                .inner
                .options
                .secrets
                .get(&secret_name(id))
                .ok()
                .flatten()
                .unwrap_or_default();
            return Some(Endpoint {
                base: base_url.clone(),
                key,
                model: model.clone(),
                cloud: *cloud,
            });
        }
        let i = self.inner.instances.lock().await.get(id).cloned()?;
        if i.info().state != InstanceState::Running {
            return None;
        }
        Some(Endpoint {
            base: format!("{}/v1", i.base_url()?),
            key: i.api_key.clone(),
            model: id.to_string(),
            cloud: false,
        })
    }

    /// Whether a model runs outside this machine.
    pub fn is_cloud(&self, id: &str) -> bool {
        self.record(id)
            .ok()
            .flatten()
            .is_some_and(|r| matches!(r.source, ModelSource::Remote { cloud: true, .. }))
    }

    /// Adds a model served by an OpenAI-compatible server – local (Ollama,
    /// LM Studio) or a cloud provider. The API key goes to the secret store.
    pub async fn add_remote(
        &self,
        url: &str,
        model: Option<&str>,
        api_key: Option<&str>,
        cloud_provider: bool,
    ) -> Result<ModelView> {
        let url = url.trim().trim_end_matches('/');
        // A provider set up as cloud counts as cloud wherever it runs.
        let cloud = cloud_provider || !is_loopback_url(url);
        let candidates: Vec<String> = if url.ends_with("/v1") || url.contains("/v1/") {
            vec![url.to_string()]
        } else {
            vec![format!("{url}/v1"), url.to_string()]
        };
        let mut found: Option<(String, Vec<String>)> = None;
        let mut last_error = String::new();
        for base in &candidates {
            let mut req = self.inner.http.get(format!("{base}/models"));
            if let Some(k) = api_key {
                req = req.bearer_auth(k);
            }
            match self
                .inner
                .http
                .send(
                    req.timeout(Duration::from_secs(20)),
                    Note::new(NetPurpose::CloudSetup, base.clone(), By::You),
                )
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let v: Value = resp.json().await.unwrap_or(Value::Null);
                    let names = v["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m["id"].as_str().map(String::from))
                        .collect();
                    found = Some((base.clone(), names));
                    break;
                }
                Ok(resp) if matches!(resp.status().as_u16(), 401 | 403) => {
                    return Err(Error::PermissionDenied(format!(
                        "{base} refused the request (HTTP {}) – check the API key",
                        resp.status().as_u16()
                    )));
                }
                Ok(resp) => last_error = format!("HTTP {}", resp.status().as_u16()),
                Err(e) => last_error = e.to_string(),
            }
        }
        let (base, names) = found.ok_or_else(|| {
            Error::unavailable(format!(
                "no OpenAI-compatible server answers at {url} ({last_error})"
            ))
        })?;
        let model = match model {
            Some(m) if names.is_empty() || names.iter().any(|n| n == m) => m.to_string(),
            Some(m) => {
                return Err(Error::not_found(format!(
                    "{base} has no model '{m}'; available: {}",
                    names.join(", ")
                )));
            }
            None if names.len() == 1 => names[0].clone(),
            None => {
                return Err(Error::invalid(format!(
                    "choose a model of {base}: {}",
                    names.join(", ")
                )));
            }
        };
        let source = ModelSource::Remote {
            base_url: base.clone(),
            model: model.clone(),
            cloud,
        };
        if let Some(existing) = self.records()?.into_iter().find(|r| {
            matches!(&r.source, ModelSource::Remote { base_url, model: m, .. } if *base_url == base && *m == model)
        }) {
            if let Some(k) = api_key {
                self.inner.options.secrets.set(&secret_name(&existing.id), k)?;
            }
            return self.status(&existing.id).await;
        }
        let mut id = slug(model.rsplit('/').next().unwrap_or(&model));
        if self.record(&id)?.is_some() {
            let host = base
                .split("://")
                .nth(1)
                .unwrap_or(&base)
                .split('/')
                .next()
                .unwrap_or("");
            id = format!("{id}-{}", slug(host));
        }
        if let Some(k) = api_key {
            self.inner.options.secrets.set(&secret_name(&id), k)?;
        }
        let embedding = looks_like_embedding(&model, None);
        let record = ModelRecord {
            id: id.clone(),
            name: model.clone(),
            source,
            files: Vec::new(),
            size_bytes: 0,
            sha256: None,
            quant: None,
            state: FileState::Ready,
            failure: None,
            plan: Plan {
                files: Vec::new(),
                quant: None,
                size_bytes: 0,
                ctx_tokens: 0,
                gpu_layers: 0,
                threads: 0,
                kv_cache_bytes: 0,
                expected_ram_bytes: 0,
                available_bytes: 0,
                fit: Fit::Fits,
                reason: format!("runs on {base} – no memory needed here"),
            },
            meta: None,
            embedding,
            found_in: None,
            autostart: false,
            pinned: false,
            added_at: Utc::now(),
        };
        self.save(&record)?;
        // A cloud model never becomes the default by itself (privacy).
        let role = if embedding { ROLE_EMBED } else { ROLE_DEFAULT };
        if !cloud && self.role_holder(role)?.is_none() {
            self.assign_role(role, &id)?;
        }
        self.inner.bus.emit(
            "model.added",
            Some(&id),
            json!({"name": model, "remote": base, "cloud": cloud}),
        );
        self.status(&id).await
    }

    /// After a daemon start: resume downloads, start models that were running.
    pub async fn restore(&self) -> Result<()> {
        let run_dir = self.inner.paths.home().join("run");
        let orphans = tokio::task::spawn_blocking(move || llama::reap_orphans(&run_dir))
            .await
            .unwrap_or_default();
        if !orphans.is_empty() {
            tracing::warn!(
                ?orphans,
                "stopped model processes left behind by a previous daemon"
            );
            self.inner
                .bus
                .emit("instance.orphans_stopped", None, json!({"pids": orphans}));
        }
        for r in self.records()? {
            match r.state {
                // Resumed after a restart: Ancilo's own doing.
                FileState::Downloading => self.spawn_download(r.clone(), r.autostart, By::Ancilo),
                FileState::Ready if r.autostart && self.resource_settings().preload() => {
                    if let Err(e) = self.start(&r.id, None).await {
                        tracing::warn!(model = %r.id, error = %e, "could not restart model");
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Stops all model processes (daemon shutdown). Keeps `autostart`.
    pub async fn shutdown(&self) {
        let all: Vec<Arc<Instance>> = self
            .inner
            .instances
            .lock()
            .await
            .drain()
            .map(|(_, i)| i)
            .collect();
        for i in all {
            i.stop().await;
        }
    }
}
