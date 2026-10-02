//! Model operations – exposed by REST, CLI, MCP and the assistant alike.

use std::path::PathBuf;

use ancilo_core::{NoInput, OpBuilder, Registry, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::{self, Address};
use crate::catalog::{Purpose, Recommendations};
use crate::health::Health;
use crate::hf::SearchHit;
use crate::manager::ResourceStatus;
use crate::manager::{HardwareView, ModelManager, ModelView, PlanPreview};
use crate::planner::{ContextSize, Wish};
use crate::resources::{Guard, Level, Priority, ResourceSettings, Variant};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddressInput {
    /// Hugging Face repository (`hf.co/org/repo`, optionally `:Q4_K_M`), GGUF file path, or server URL.
    pub address: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetResourcesInput {
    /// Start from this level's preset (default: the current settings).
    #[serde(default)]
    pub level: Option<Level>,
    /// Unload a model after this many seconds without use.
    #[serde(default)]
    pub keep_loaded_secs: Option<u64>,
    /// `true`: keep models loaded (never unload when idle).
    #[serde(default)]
    pub keep_loaded_always: Option<bool>,
    /// Share of the computer's memory Ancilo's models may use (0.1–0.95).
    #[serde(default)]
    pub max_share: Option<f64>,
    #[serde(default)]
    pub parallel: Option<u32>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub variant: Option<Variant>,
    #[serde(default)]
    pub guard: Option<Guard>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Unloaded {
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendInput {
    /// What the models are for: chat, code, documents (default: chat).
    #[serde(default)]
    pub purposes: Vec<Purpose>,
    /// Fetch the newest model list from the Ancilo repository first (network).
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    /// Words to search for, e.g. `qwen coder`.
    pub query: String,
    /// At most this many results (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    pub address: String,
    /// Context size: small (8k), medium (32k), large (128k). Default: medium.
    #[serde(default)]
    pub context: Option<ContextSize>,
    /// Only use this quantization, e.g. `Q4_K_M`.
    #[serde(default)]
    pub quant: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddInput {
    pub address: String,
    #[serde(default)]
    pub context: Option<ContextSize>,
    #[serde(default)]
    pub quant: Option<String>,
    /// Start the model once it is available. Default: true.
    #[serde(default)]
    pub start: Option<bool>,
    /// Server addresses: which model of the server (if it offers several).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// Model id, unique id prefix or name.
    pub model: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartInput {
    pub model: String,
    #[serde(default)]
    pub context: Option<ContextSize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveInput {
    pub model: String,
    /// Keep downloaded files on disk. Default: false.
    #[serde(default)]
    pub keep_files: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RemoveOutput {
    pub removed: String,
    pub deleted_files: Vec<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloudInput {
    /// OpenAI-compatible API base, e.g. `https://api.deepinfra.com/v1/openai`.
    pub base_url: String,
    /// The provider's model name, e.g. `Qwen/Qwen3-235B-A22B-Instruct-2507`.
    pub model: String,
    /// Stored in the system keychain – never in files, logs or events.
    pub api_key: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CloudOutput {
    pub model: ModelView,
    /// What leaves this machine when the model is used.
    pub privacy: String,
}

pub const CLOUD_PRIVACY: &str = "Prompts and answers of the roles you give this model are sent to the provider. It gets no role by itself – assign one explicitly (e.g. `ancilo assign assistant <model>`). Code from your projects is never sent: delegation, comparisons, search embeddings and the coding agent refuse cloud models.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinInput {
    pub model: String,
    /// Pinned models stay loaded (never unloaded to make room).
    pub pinned: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleInput {
    pub role: String,
    pub model: String,
}

fn wish(context: Option<ContextSize>, quant: Option<String>) -> Wish {
    Wish {
        context: context.unwrap_or_default(),
        quant,
        file: None,
        remote_model: None,
    }
}

pub fn register(registry: &mut Registry, manager: ModelManager) {
    registry.register(
        OpBuilder::new("resolve_address")
            .summary("Explain what a model address refers to")
            .description("Classifies a model address: Hugging Face repository (with optional file or quantization), local GGUF file, or server URL. No network access.")
            .handler(|_ctx, i: AddressInput| async move { address::resolve(&i.address) as Result<Address> }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("hardware_info")
            .summary("Show this machine's hardware and memory available for models")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.hardware_view().await as Result<HardwareView> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("resource_status")
            .summary("How much of the computer Ancilo uses and may use – memory, heat, loaded models")
            .description("The resource settings (level and single values), the four levels' presets, the computer's state right now (free memory, memory pressure, heat, swap), the memory cap for models, the loaded models with the time until they are unloaded, and recent unloads by the guard.")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.resource_status().await as Result<ResourceStatus> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("set_resources")
            .summary("Set how much of the computer Ancilo may take (eco, balanced, performance, max, or single values)")
            .description("`level` applies a preset; single values on top make it custom. Lower levels unload idle models sooner, cap memory lower, run models at lower priority and prefer smaller variants; every level except max refuses to load a model into memory the computer does not have free.")
            .manage()
            .handler(move |_ctx, i: SetResourcesInput| {
                let m = m.clone();
                async move {
                    let mut s = match i.level {
                        Some(l) => ResourceSettings::preset(l),
                        None => m.resource_settings(),
                    };
                    if let Some(v) = i.keep_loaded_secs {
                        s.keep_loaded_secs = Some(v);
                    }
                    if i.keep_loaded_always == Some(true) {
                        s.keep_loaded_secs = None;
                    }
                    if let Some(v) = i.max_share {
                        s.max_share = v;
                    }
                    if let Some(v) = i.parallel {
                        s.parallel = v;
                    }
                    if let Some(v) = i.priority {
                        s.priority = v;
                    }
                    if let Some(v) = i.variant {
                        s.variant = v;
                    }
                    if let Some(v) = i.guard {
                        s.guard = v;
                    }
                    m.set_resources(s) as Result<ResourceSettings>
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("unload_models")
            .summary("Unload every model that is not answering right now (frees memory)")
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.unload_all().await.map(|models| Unloaded { models }) as Result<Unloaded> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("system_health")
            .summary("Is the computer about to be saturated – memory, processor, heat – who causes it, and what helps")
            .description("The system monitor's verdict: ok, tight (the computer may get slow) or critical (about to be unusable), the causes (memory, cpu, heat), memory and processor use now, what Ancilo's models take, whether other programs take most of it, the biggest other programs (only while it is tight) and the fixes that help (unload_models, set_resources level eco, open_activity_monitor). Names local programs: not for cloud models.")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.health().await as Result<Health> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("open_activity_monitor")
            .summary("Open the Activity Monitor, where other programs can be closed")
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move {
                    m.open_activity_monitor().map(|opened| Opened { opened }) as Result<Opened>
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("recommend_models")
            .summary("Which models suit this machine – ranked, with size, speed and whether they fit right now")
            .description("Picks from Ancilo's model list what runs well here for the given purposes (chat, code, documents): the best choice, two or three alternatives and everything else that fits. Considers the memory models may use and what other programs leave free right now; estimates speed from the chip; prefers installed models. `address` of a suggestion goes straight into add_model.")
            .handler(move |_ctx, i: RecommendInput| {
                let m = m.clone();
                async move { m.recommend(&i.purposes, i.refresh).await as Result<Recommendations> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("search_models")
            .summary("Search Hugging Face for GGUF models (most downloaded first)")
            .description("For models outside Ancilo's list. Check a result with plan_model before adding it.")
            .handler(move |_ctx, i: SearchInput| {
                let m = m.clone();
                async move { m.search(&i.query, i.limit.unwrap_or(20)).await as Result<Vec<SearchHit>> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("plan_model")
            .summary("Show how a model would run on this machine, without adding it")
            .description("Resolves the address, picks the best quantization and context for this hardware, and reports whether it fits, what would be downloaded and whether an existing local copy (LM Studio, Ollama, Hugging Face cache) can be used.")
            .handler(move |_ctx, i: PlanInput| {
                let m = m.clone();
                async move { m.plan(&i.address, &wish(i.context, i.quant)).await as Result<PlanPreview> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("add_model")
            .summary("Add a model (download if needed) and start it")
            .description("One step from an address to a running model: plans for this hardware, reuses an existing local copy or downloads with checksum verification, then starts it. Returns immediately; follow progress via events (download.progress, model.running).")
            .manage()
            .consequential()
            .handler(move |_ctx, i: AddInput| {
                let m = m.clone();
                async move {
                    let w = Wish { remote_model: i.model, ..wish(i.context, i.quant) };
                    m.add(&i.address, &w, i.start.unwrap_or(true)).await as Result<ModelView>
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("list_models")
            .summary("List all models with their status")
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.list().await as Result<Vec<ModelView>> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("model_status")
            .summary("Show the status of one model")
            .handler(move |_ctx, i: ModelRef| {
                let m = m.clone();
                async move { m.status(&i.model).await as Result<ModelView> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("start_model")
            .summary("Load a model into memory")
            .description("Starts a model that is in the library. Fails with insufficient_resources if the memory budget would be exceeded.")
            .manage()
            .handler(move |_ctx, i: StartInput| {
                let m = m.clone();
                async move { m.start(&i.model, i.context).await as Result<ModelView> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("stop_model")
            .summary("Unload a model from memory")
            .manage()
            .handler(move |_ctx, i: ModelRef| {
                let m = m.clone();
                async move { m.stop(&i.model).await as Result<ModelView> }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("remove_model")
            .summary("Remove a model from the library")
            .description("Stops the model and removes it. Files downloaded by Ancilo are deleted unless keep_files is set; files belonging to other tools are never deleted.")
            .manage()
            .consequential()
            .handler(move |_ctx, i: RemoveInput| {
                let m = m.clone();
                async move {
                    let deleted = m.remove(&i.model, i.keep_files).await?;
                    Ok(RemoveOutput { removed: i.model, deleted_files: deleted })
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("retry_download")
            .summary("Resume a download that failed")
            .manage()
            .handler(move |_ctx, i: ModelRef| {
                let m = m.clone();
                async move { m.retry_download(&i.model).await }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("set_cloud_provider")
            .summary("Add a model of a cloud provider (OpenAI-compatible, e.g. DeepInfra, OpenRouter)")
            .description("Adds the provider's model like a local one; the API key goes to the system keychain. Data leaves this machine only for roles you explicitly give the model; code never does.")
            .manage()
            .consequential()
            .handler(move |_ctx, i: CloudInput| {
                let m = m.clone();
                async move {
                    let model = m.add_remote(&i.base_url, Some(&i.model), Some(&i.api_key), true).await?;
                    Ok(CloudOutput { model, privacy: CLOUD_PRIVACY.into() })
                }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("install_llama")
            .summary(
                "Download and install the pinned llama.cpp build (packages already contain it)",
            )
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let m = m.clone();
                async move { m.install_llama().await }
            }),
    );
    let m = manager.clone();
    registry.register(
        OpBuilder::new("set_pinned")
            .summary("Keep a model loaded (pinned) or let it be unloaded when memory is needed")
            .manage()
            .handler(move |_ctx, i: PinInput| {
                let m = m.clone();
                async move {
                    m.set_pinned(&i.model, i.pinned)?;
                    m.status(&i.model).await as Result<ModelView>
                }
            }),
    );
    let m = manager;
    registry.register(
        OpBuilder::new("assign_role")
            .summary("Use a model for a role: default, delegation, coding, assistant, embed")
            .description("Roles: `default` serves everything without a more specific role; `delegation` does tasks delegated by Claude Code, Codex and `ancilo run`; `coding` is the app's coding agent; `assistant` answers `ask`; `embed` computes embeddings for search.")
            .manage()
            .handler(move |_ctx, i: RoleInput| {
                let m = m.clone();
                async move {
                    let r = m.find(&i.model)?;
                    m.assign_role(&i.role, &r.id)?;
                    m.status(&r.id).await as Result<ModelView>
                }
            }),
    );
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Opened {
    /// Whether it was opened (false in tests).
    pub opened: bool,
}
