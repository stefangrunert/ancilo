//! The model catalog: models Ancilo recommends, and which of them suit this
//! machine – for people who know nothing about models.
//!
//! The list ships with Ancilo (`knowledge/catalog.json`); a newer one can be
//! fetched from the Ancilo repository when the user opens the model choice.
//! Choosing is pure: hardware, memory free right now, the purposes and the
//! installed models go in; ranked suggestions come out.

use std::collections::BTreeMap;

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::{self, Address};
use crate::hardware::{GIB, Gpu, HardwareProfile};
use crate::planner::{self, ContextSize, Fit, RepoFile, Wish};
use crate::quant;
use crate::resources::Variant;

/// The catalog that ships with this version.
pub const BUNDLED: &str = include_str!("../../../knowledge/catalog.json");

/// Download speed assumed for time estimates (50 Mbit/s).
pub const ASSUMED_DOWNLOAD_BYTES_PER_SEC: u64 = 6_250_000;
/// Kept free for the system on top of what other programs use right now.
const SAFETY_BYTES: u64 = GIB;
/// Answers below this many tokens per second feel broken to most people.
const SLOW_BELOW: f64 = 10.0;
const FAST_FROM: f64 = 25.0;

/// What someone wants to do with Ancilo.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Chat and writing.
    Chat,
    /// Programming.
    Code,
    /// Working with one's own documents (needs an embedding model too).
    Documents,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Localized {
    pub de: String,
    pub en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    /// Also the start of the model id Ancilo gives the installed model.
    pub id: String,
    pub name: String,
    /// Hugging Face address (`hf.co/org/repo`).
    pub address: String,
    pub maker: String,
    /// `chat` or `embedding`.
    pub kind: String,
    pub purposes: Vec<Purpose>,
    pub params_b: f64,
    /// Parameters used per token (smaller than `params_b` for mixture-of-experts models).
    pub active_params_b: f64,
    /// Ancilo's estimate per purpose, 1 (basic) to 10 (excellent).
    #[serde(default)]
    pub quality: BTreeMap<Purpose, u8>,
    /// Download size per quantization offered (bytes).
    pub sizes: BTreeMap<String, u64>,
    pub license: String,
    /// `YYYY-MM`
    pub released: String,
    /// Measured by Ancilo's own evals.
    #[serde(default)]
    pub tested: bool,
    pub summary: Localized,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    /// `YYYY-MM-DD` – the newer catalog wins.
    pub updated: String,
    #[serde(default)]
    pub note: String,
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    pub fn bundled() -> Self {
        Self::parse(BUNDLED).expect("the bundled catalog is valid")
    }

    /// Reads and checks a catalog (a fetched one may be broken).
    pub fn parse(text: &str) -> Result<Self> {
        let c: Catalog = serde_json::from_str(text)
            .map_err(|e| Error::invalid(format!("model catalog: {e}")))?;
        if c.version != 1 {
            return Err(Error::invalid(format!(
                "model catalog version {} is not supported",
                c.version
            )));
        }
        if c.models.is_empty() || c.updated.len() != 10 {
            return Err(Error::invalid("model catalog is empty or undated"));
        }
        for m in &c.models {
            let hf = matches!(
                address::resolve(&m.address),
                Ok(Address::HuggingFace { .. })
            );
            let ok = hf
                && !m.sizes.is_empty()
                && m.sizes
                    .keys()
                    .all(|q| quant::from_file_name(&format!("x-{q}.gguf")).is_some())
                && m.active_params_b > 0.0
                && (m.kind == "embedding"
                    || (m.kind == "chat"
                        && !m.purposes.is_empty()
                        && m.purposes.iter().all(|p| m.quality.contains_key(p))));
            if !ok {
                return Err(Error::invalid(format!(
                    "model catalog: entry '{}' is incomplete",
                    m.id
                )));
            }
        }
        Ok(c)
    }
}

/// Is there room for a model right now?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Room {
    /// Fits next to what is open now, with room to spare.
    Comfortable,
    /// Fits this machine, but other programs would have to be closed.
    ClosePrograms,
    /// Too big for this machine.
    TooBig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Fast,
    Ok,
    Slow,
}

/// A catalog model as it would run on this machine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Suggestion {
    pub id: String,
    pub name: String,
    /// What to pass to `add_model` (with the chosen quantization).
    pub address: String,
    pub maker: String,
    pub summary: Localized,
    pub license: String,
    pub released: String,
    pub tested: bool,
    pub purposes: Vec<Purpose>,
    /// Ancilo's estimate for the purposes asked, 1–10.
    pub quality: u8,
    pub quant: String,
    pub size_bytes: u64,
    /// 0 when the model is already installed.
    pub download_bytes: u64,
    /// At 50 Mbit/s (0 when nothing is downloaded).
    pub download_minutes: u32,
    pub ram_bytes: u64,
    pub speed: Speed,
    /// Estimated from the chip's memory bandwidth, or measured when installed.
    pub tokens_per_sec: f64,
    pub measured: bool,
    pub room: Room,
    /// The installed model's id.
    pub installed: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryView {
    pub total_bytes: u64,
    /// What models may use at most (GPU limit, minus the reserve for the system).
    pub for_models_bytes: u64,
    /// Free right now (other programs taken into account) – unknown in tests.
    pub available_now_bytes: Option<u64>,
    /// Room for a model at the moment (what other programs leave, plus what
    /// Ancilo's own loaded models would free).
    pub room_now_bytes: u64,
    pub used_by_ancilo_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CatalogInfo {
    pub updated: String,
    /// `bundled` or `online`
    pub source: String,
    /// Why a newer list could not be fetched (the bundled one is used).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Recommendations {
    pub purposes: Vec<Purpose>,
    pub best: Option<Suggestion>,
    /// Two or three other good choices.
    pub alternatives: Vec<Suggestion>,
    /// Everything else that runs here, best first.
    pub more: Vec<Suggestion>,
    /// Catalog models too big for this machine.
    pub too_big: u32,
    /// Needed to work with documents (when none is installed yet).
    pub embedding: Option<Suggestion>,
    pub chip: String,
    pub memory: MemoryView,
    pub catalog: CatalogInfo,
}

/// An installed model, as far as recommendations care.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: String,
    pub embedding: bool,
    pub tokens_per_sec: Option<f64>,
}

/// Memory bandwidth (GB/s) of the chip – what decides how fast a model answers.
pub fn memory_bandwidth_gbps(hw: &HardwareProfile) -> f64 {
    let chip = hw.chip.to_ascii_lowercase();
    let generation = (1..=9)
        .rev()
        .find(|g| chip.contains(&format!("m{g}")))
        .unwrap_or(0);
    let tier = if chip.contains("ultra") {
        3
    } else if chip.contains("max") {
        2
    } else if chip.contains("pro") {
        1
    } else {
        0
    };
    // Published figures; newer generations extrapolated from their base chip.
    let known: &[(u32, [f64; 4])] = &[
        (1, [68.0, 200.0, 400.0, 800.0]),
        (2, [100.0, 200.0, 400.0, 800.0]),
        (3, [100.0, 150.0, 400.0, 819.0]),
        (4, [120.0, 273.0, 546.0, 819.0]),
        (5, [153.0, 307.0, 614.0, 1228.0]),
    ];
    match (hw.gpu, generation) {
        (Gpu::Metal, g) if g > 0 => known
            .iter()
            .find(|(k, _)| *k == g)
            .or(known.last())
            .map(|(_, bw)| bw[tier])
            .unwrap_or(100.0),
        // An Apple chip of unknown generation.
        (Gpu::Metal, _) => 100.0,
        (Gpu::Cuda, _) => 300.0,
        (Gpu::None, _) => 50.0,
    }
}

/// Tokens per second to expect: generating reads all active weights once per token.
pub fn estimate_tokens_per_sec(hw: &HardwareProfile, active_params_b: f64, bits: f64) -> f64 {
    let bytes = active_params_b * 1e9 * bits / 8.0;
    0.6 * memory_bandwidth_gbps(hw) * 1e9 / bytes.max(1.0)
}

pub fn speed_of(tokens_per_sec: f64) -> Speed {
    if tokens_per_sec >= FAST_FROM {
        Speed::Fast
    } else if tokens_per_sec >= SLOW_BELOW {
        Speed::Ok
    } else {
        Speed::Slow
    }
}

/// The machine's memory as recommendations see it.
pub fn memory_view(
    hw: &HardwareProfile,
    for_models: u64,
    available_now: Option<u64>,
    used_by_ancilo: u64,
) -> MemoryView {
    let room_now = match available_now {
        Some(a) => (a + used_by_ancilo)
            .saturating_sub(SAFETY_BYTES)
            .min(for_models),
        None => for_models,
    };
    MemoryView {
        total_bytes: hw.total_ram_bytes,
        for_models_bytes: for_models,
        available_now_bytes: available_now,
        room_now_bytes: room_now,
        used_by_ancilo_bytes: used_by_ancilo,
    }
}

fn installed_as<'a>(m: &CatalogModel, installed: &'a [Installed]) -> Option<&'a Installed> {
    installed
        .iter()
        .find(|i| i.id == m.id || i.id.starts_with(&format!("{}-", m.id)))
}

/// How one catalog model would run here, in its best fitting quantization.
fn suggest(
    m: &CatalogModel,
    purposes: &[Purpose],
    hw: &HardwareProfile,
    memory: &MemoryView,
    reserve: u64,
    installed: &[Installed],
    variant: Variant,
) -> Option<Suggestion> {
    let mine = installed_as(m, installed);
    let quality = if m.kind == "embedding" {
        0
    } else {
        let asked: Vec<u8> = purposes
            .iter()
            .map(|p| m.quality.get(p).copied().unwrap_or(1))
            .collect();
        (asked.iter().map(|q| *q as f64).sum::<f64>() / asked.len().max(1) as f64).round() as u8
    };
    // The variant that runs best: room first, then speed, then precision
    // (Q8 only when it is not noticeably slower than Q4).
    let mut options: Vec<(String, u64, f64)> = m
        .sizes
        .iter()
        .filter_map(|(q, size)| {
            let bits = quant::from_file_name(&format!("x-{q}.gguf"))?.bits;
            Some((q.clone(), *size, bits))
        })
        .collect();
    options.sort_by(|a, b| b.2.total_cmp(&a.2));
    if variant == Variant::Small {
        // Only the smallest: half the memory, little less quality.
        options = options.split_off(options.len().saturating_sub(1));
    }
    let mut best: Option<Suggestion> = None;
    for (q, size, bits) in options {
        let file = RepoFile {
            path: format!("{}-{q}.gguf", m.id),
            size,
            sha256: None,
        };
        let wish = Wish {
            context: if m.kind == "embedding" {
                ContextSize::Small
            } else {
                ContextSize::Medium
            },
            ..Default::default()
        };
        let Ok(plan) = planner::plan(hw, &[file], None, &wish, 0, reserve) else {
            continue;
        };
        let room = match plan.fit {
            Fit::DoesNotFit => Room::TooBig,
            Fit::Tight => Room::ClosePrograms,
            Fit::Fits if plan.expected_ram_bytes as f64 > memory.room_now_bytes as f64 * 0.9 => {
                Room::ClosePrograms
            }
            Fit::Fits => Room::Comfortable,
        };
        let (tokens_per_sec, measured) = match mine.and_then(|i| i.tokens_per_sec) {
            Some(t) => (t, true),
            None => (estimate_tokens_per_sec(hw, m.active_params_b, bits), false),
        };
        let download = if mine.is_some() { 0 } else { size };
        let s = Suggestion {
            id: m.id.clone(),
            name: m.name.clone(),
            address: format!("{}:{q}", m.address),
            maker: m.maker.clone(),
            summary: m.summary.clone(),
            license: m.license.clone(),
            released: m.released.clone(),
            tested: m.tested,
            purposes: m.purposes.clone(),
            quality,
            quant: q,
            size_bytes: size,
            download_bytes: download,
            download_minutes: if download == 0 {
                0
            } else {
                download
                    .div_ceil(ASSUMED_DOWNLOAD_BYTES_PER_SEC * 60)
                    .max(1) as u32
            },
            ram_bytes: plan.expected_ram_bytes,
            speed: speed_of(tokens_per_sec),
            tokens_per_sec: (tokens_per_sec * 10.0).round() / 10.0,
            measured,
            room,
            installed: mine.map(|i| i.id.clone()),
        };
        // Options come with more bits first: a later one only wins when it
        // runs better ("precise": only when the first does not fit or is slow).
        let key = |x: &Suggestion| match variant {
            Variant::Precise => {
                let (room, speed, _) = usability(x);
                (room, u8::from(speed > 0), true)
            }
            _ => usability(x),
        };
        if best.as_ref().is_none_or(|b| key(&s) > key(b)) {
            best = Some(s);
        }
    }
    best
}

/// Answers at this pace read comfortably.
const SMOOTH_FROM: f64 = 15.0;

fn usability(s: &Suggestion) -> (u8, u8, bool) {
    let room = match s.room {
        Room::Comfortable => 2,
        Room::ClosePrograms => 1,
        Room::TooBig => 0,
    };
    let speed = match s.speed {
        Speed::Fast => 2,
        Speed::Ok => 1,
        Speed::Slow => 0,
    };
    (room, speed, s.tokens_per_sec >= SMOOTH_FROM)
}

/// Comfortable and not slow always beats everything else; within that,
/// quality decides, then speed.
fn score(s: &Suggestion) -> i32 {
    let speed = match s.speed {
        Speed::Fast => 10,
        Speed::Ok => 0,
        Speed::Slow => -200,
    };
    let room = match s.room {
        Room::Comfortable => 0,
        Room::ClosePrograms => -200,
        Room::TooBig => -1000,
    };
    s.quality as i32 * 10
        + speed
        + (s.tokens_per_sec.min(40.0) / 4.0) as i32
        + room
        + if s.installed.is_some() { 6 } else { 0 }
        + if s.tested { 2 } else { 0 }
}

/// Which catalog models to use on this machine, best first.
pub fn recommend(
    catalog: &Catalog,
    purposes: &[Purpose],
    hw: &HardwareProfile,
    memory: MemoryView,
    reserve: u64,
    installed: &[Installed],
    variant: Variant,
) -> (
    Option<Suggestion>,
    Vec<Suggestion>,
    Vec<Suggestion>,
    u32,
    Option<Suggestion>,
) {
    let purposes: Vec<Purpose> = if purposes.is_empty() {
        vec![Purpose::Chat]
    } else {
        purposes.to_vec()
    };
    let mut fitting = Vec::new();
    let mut too_big = 0;
    for m in catalog
        .models
        .iter()
        .filter(|m| m.kind == "chat" && m.purposes.iter().any(|p| purposes.contains(p)))
    {
        match suggest(m, &purposes, hw, &memory, reserve, installed, variant) {
            Some(s) if s.room != Room::TooBig => fitting.push(s),
            _ => too_big += 1,
        }
    }
    fitting.sort_by(|a, b| score(b).cmp(&score(a)).then(a.ram_bytes.cmp(&b.ram_bytes)));
    let mut rest = fitting.into_iter();
    let best = rest.next();
    let alternatives: Vec<Suggestion> = rest.by_ref().take(3).collect();
    let more: Vec<Suggestion> = rest.collect();
    let embedding = (purposes.contains(&Purpose::Documents)
        && !installed.iter().any(|i| i.embedding))
    .then(|| {
        catalog
            .models
            .iter()
            .filter(|m| m.kind == "embedding")
            .find_map(|m| suggest(m, &purposes, hw, &memory, reserve, installed, Variant::Auto))
    })
    .flatten();
    (best, alternatives, more, too_big, embedding)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(hw: &HardwareProfile, available_now: Option<u64>) -> MemoryView {
        let reserve = (hw.total_ram_bytes / 5).min(8 * GIB);
        let budget = hw
            .gpu_memory_bytes
            .unwrap_or(u64::MAX)
            .min(hw.total_ram_bytes - reserve);
        memory_view(hw, budget, available_now, 0)
    }

    fn run(
        ram_gib: u64,
        purposes: &[Purpose],
        available_now: Option<u64>,
    ) -> (
        Option<Suggestion>,
        Vec<Suggestion>,
        Vec<Suggestion>,
        u32,
        Option<Suggestion>,
    ) {
        let hw = HardwareProfile::apple(ram_gib);
        let reserve = (hw.total_ram_bytes / 5).min(8 * GIB);
        recommend(
            &Catalog::bundled(),
            purposes,
            &hw,
            mem(&hw, available_now),
            reserve,
            &[],
            Variant::Auto,
        )
    }

    #[test]
    fn the_bundled_catalog_is_valid() {
        let c = Catalog::bundled();
        assert!(c.models.len() >= 10);
        assert!(c.models.iter().any(|m| m.kind == "embedding"));
        // Every purpose has small models (for small machines).
        for p in [Purpose::Chat, Purpose::Code, Purpose::Documents] {
            assert!(
                c.models.iter().any(|m| m.purposes.contains(&p)
                    && m.sizes.values().min().copied().unwrap_or(u64::MAX) < 5_000_000_000),
                "{p:?}"
            );
        }
        let mut ids: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.models.len(), "ids are unique");
    }

    #[test]
    fn broken_catalogs_are_refused() {
        assert!(Catalog::parse("{}").is_err());
        let mut c: serde_json::Value = serde_json::from_str(BUNDLED).unwrap();
        c["models"][0]["sizes"] = serde_json::json!({});
        assert!(Catalog::parse(&c.to_string()).is_err());
        let mut c: serde_json::Value = serde_json::from_str(BUNDLED).unwrap();
        c["version"] = 2.into();
        assert!(Catalog::parse(&c.to_string()).is_err());
    }

    #[test]
    fn bandwidth_follows_the_chip() {
        let mut hw = HardwareProfile::apple(36);
        hw.chip = "Apple M4 Pro".into();
        assert_eq!(memory_bandwidth_gbps(&hw), 273.0);
        hw.chip = "Apple M1".into();
        assert_eq!(memory_bandwidth_gbps(&hw), 68.0);
        hw.chip = "Apple M5 Max".into();
        assert!(memory_bandwidth_gbps(&hw) > 546.0);
        hw.gpu = Gpu::None;
        assert_eq!(memory_bandwidth_gbps(&hw), 50.0);
    }

    // covers: M1-AC-13
    /// Small machines get small models, large machines strong ones – never
    /// something that does not fit or answers too slowly.
    #[test]
    fn recommendations_match_the_machine() {
        let mut seen = Vec::new();
        for ram in [8u64, 16, 32, 64, 128] {
            let (best, alternatives, _, _, _) = run(ram, &[Purpose::Chat], None);
            let best = best.unwrap_or_else(|| panic!("{ram} GB: no recommendation"));
            assert_eq!(best.room, Room::Comfortable, "{ram} GB: {}", best.name);
            assert_ne!(best.speed, Speed::Slow, "{ram} GB: {}", best.name);
            assert!(best.address.starts_with("hf.co/") && best.address.contains(':'));
            assert!(best.download_minutes >= 1);
            assert!(alternatives.len() <= 3);
            seen.push((ram, best.quality, best.ram_bytes));
        }
        // More memory never means a worse recommendation.
        for w in seen.windows(2) {
            assert!(w[1].1 >= w[0].1, "{seen:?}");
        }
        assert!(seen[0].2 < 6 * GIB, "8 GB machine: {seen:?}");
        assert!(seen.last().unwrap().1 >= 8, "128 GB machine: {seen:?}");
    }

    #[test]
    fn what_is_open_right_now_counts() {
        let (free, ..) = run(32, &[Purpose::Chat], None);
        // Other programs leave only 6 GB: a smaller model, or a warning.
        let (busy, alternatives, ..) = run(32, &[Purpose::Chat], Some(6 * GIB));
        let busy = busy.unwrap();
        assert!(busy.ram_bytes < free.unwrap().ram_bytes);
        assert!(busy.ram_bytes <= 5 * GIB, "{}", busy.name);
        assert!(alternatives.iter().all(|a| a.room != Room::TooBig));
    }

    #[test]
    fn purposes_pick_the_right_kind() {
        let (code, ..) = run(64, &[Purpose::Code], None);
        let code = code.unwrap();
        assert!(code.purposes.contains(&Purpose::Code), "{}", code.name);
        // Documents bring the embedding model along.
        let (_, _, _, _, embedding) = run(16, &[Purpose::Documents], None);
        assert_eq!(embedding.unwrap().id, "all-minilm-l6-v2");
        let (_, _, _, _, none) = run(16, &[Purpose::Chat], None);
        assert!(none.is_none());
    }

    #[test]
    fn installed_models_are_preferred_and_cost_no_download() {
        let hw = HardwareProfile::apple(64);
        let reserve = 8 * GIB;
        let installed = [Installed {
            id: "qwen3.6-35b-a3b-q8_0".into(),
            embedding: false,
            tokens_per_sec: Some(42.0),
        }];
        let (best, ..) = recommend(
            &Catalog::bundled(),
            &[Purpose::Chat],
            &hw,
            mem(&hw, None),
            reserve,
            &installed,
            Variant::Auto,
        );
        let best = best.unwrap();
        assert_eq!(best.installed.as_deref(), Some("qwen3.6-35b-a3b-q8_0"));
        assert_eq!(best.download_bytes, 0);
        assert_eq!(best.download_minutes, 0);
        assert!(best.measured && best.tokens_per_sec == 42.0);
    }

    #[test]
    fn the_variant_setting_picks_small_or_precise() {
        let hw = HardwareProfile::apple(64);
        let reserve = 8 * GIB;
        let pick = |v| {
            recommend(
                &Catalog::bundled(),
                &[Purpose::Chat],
                &hw,
                mem(&hw, None),
                reserve,
                &[],
                v,
            )
            .0
            .unwrap()
        };
        let small = pick(Variant::Small);
        assert!(small.quant.contains("Q4"), "{}", small.quant);
        let precise = pick(Variant::Precise);
        assert!(precise.ram_bytes >= small.ram_bytes);
    }

    #[test]
    fn a_tiny_machine_still_gets_something_or_nothing_honestly() {
        let (best, _, _, too_big, _) = run(4, &[Purpose::Chat], None);
        let best = best.expect("a small model fits 4 GB");
        assert!(best.ram_bytes < 3 * GIB);
        assert!(too_big > 5);
    }
}
