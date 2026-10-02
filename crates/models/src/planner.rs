//! The planner: from hardware, the files of a repository and a few wishes to a
//! concrete, runnable configuration – or an honest "does not fit".
//!
//! Pure function: no I/O, fully table-testable.

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hardware::{GIB, Gpu, HardwareProfile};
use crate::quant::{self, Quant};

/// A file of a model repository (or a local file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
}

/// User-facing context size. German labels in the UI: klein, mittel, groß.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl ContextSize {
    pub fn tokens(self) -> u64 {
        match self {
            ContextSize::Small => 8 * 1024,
            ContextSize::Medium => 32 * 1024,
            ContextSize::Large => 128 * 1024,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "small" | "klein" | "s" => Some(Self::Small),
            "medium" | "mittel" | "m" => Some(Self::Medium),
            "large" | "groß" | "gross" | "l" => Some(Self::Large),
            _ => None,
        }
    }
}

/// Architecture facts needed to estimate the KV cache.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelShape {
    pub context_length: Option<u64>,
    pub block_count: Option<u64>,
    pub head_count_kv: Option<u64>,
    pub head_dim: Option<u64>,
}

impl From<&crate::gguf::GgufMeta> for ModelShape {
    fn from(m: &crate::gguf::GgufMeta) -> Self {
        let head_dim = m
            .key_length
            .or_else(|| match (m.embedding_length, m.head_count) {
                (Some(e), Some(h)) if h > 0 => Some(e / h),
                _ => None,
            });
        Self {
            context_length: m.context_length,
            block_count: m.block_count,
            head_count_kv: m.head_count_kv.or(m.head_count),
            head_dim,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Wish {
    #[serde(default)]
    pub context: ContextSize,
    /// Only consider this quantization (`Q4_K_M`, with or without `UD-`).
    pub quant: Option<String>,
    /// Only consider this file.
    pub file: Option<String>,
    /// For server addresses: which of the server's models.
    #[serde(default)]
    pub remote_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Comfortable headroom.
    Fits,
    /// Fits, but little memory left for other apps.
    Tight,
    DoesNotFit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    /// One file, or all parts of a split model in order.
    pub files: Vec<RepoFile>,
    pub quant: Option<String>,
    pub size_bytes: u64,
    pub ctx_tokens: u64,
    /// `-1`: all layers on the GPU.
    pub gpu_layers: i32,
    pub threads: u32,
    pub kv_cache_bytes: u64,
    /// Model + KV cache + compute buffers.
    pub expected_ram_bytes: u64,
    /// Memory available for this model when planned.
    pub available_bytes: u64,
    pub fit: Fit,
    /// Human explanation of the choice.
    pub reason: String,
}

impl Plan {
    pub fn primary_file(&self) -> &RepoFile {
        &self.files[0]
    }
}

/// A runnable model file (possibly split into parts).
#[derive(Debug, Clone)]
struct Candidate {
    files: Vec<RepoFile>,
    quant: Option<Quant>,
    size: u64,
}

fn candidates(files: &[RepoFile]) -> Vec<Candidate> {
    let mut singles = Vec::new();
    let mut splits: std::collections::BTreeMap<String, Vec<(u32, u32, RepoFile)>> =
        Default::default();
    for f in files.iter().filter(|f| !quant::is_auxiliary(&f.path)) {
        match quant::split_part(&f.path) {
            Some((part, total)) => {
                let lower = f.path.to_ascii_lowercase();
                let key = lower[..lower
                    .rfind(&format!("-{part:05}-of-"))
                    .unwrap_or(lower.len())]
                    .to_string();
                splits
                    .entry(key)
                    .or_default()
                    .push((part, total, f.clone()));
            }
            None => singles.push(Candidate {
                quant: quant::from_file_name(&f.path),
                size: f.size,
                files: vec![f.clone()],
            }),
        }
    }
    for (_, mut parts) in splits {
        parts.sort_by_key(|p| p.0);
        let total = parts[0].1;
        // Only complete sets.
        if parts.len() as u32 != total || parts.iter().enumerate().any(|(i, p)| p.0 != i as u32 + 1)
        {
            continue;
        }
        let files: Vec<RepoFile> = parts.into_iter().map(|p| p.2).collect();
        singles.push(Candidate {
            quant: quant::from_file_name(&files[0].path),
            size: files.iter().map(|f| f.size).sum(),
            files,
        });
    }
    singles
}

/// Bytes of KV cache per token of context.
fn kv_bytes_per_token(shape: Option<&ModelShape>, model_size: u64) -> u64 {
    if let Some(s) = shape
        && let (Some(layers), Some(heads), Some(dim)) = (s.block_count, s.head_count_kv, s.head_dim)
    {
        // K and V, f16 (2 bytes).
        return 2 * layers * heads * dim * 2;
    }
    // Conservative fallback, scaled with model size (≈ 30B model → ~190 KiB/token).
    (model_size / GIB).clamp(2, 64) * 6 * 1024
}

/// Upper bound on quality worth its speed: beyond ~8.5 bits per weight quality
/// barely improves while tokens/s drop (memory bandwidth bound).
const MAX_USEFUL_BITS: f64 = 8.6;

/// Plans how to run a model.
///
/// `used_bytes` is memory already taken by other loaded models; `reserve_bytes`
/// stays free for the operating system and other applications.
pub fn plan(
    hw: &HardwareProfile,
    files: &[RepoFile],
    shape: Option<&ModelShape>,
    wish: &Wish,
    used_bytes: u64,
    reserve_bytes: u64,
) -> Result<Plan> {
    plan_with_ctx(
        hw,
        files,
        shape,
        wish,
        wish.context.tokens(),
        used_bytes,
        reserve_bytes,
    )
}

/// Like [`plan`], with an exact context size in tokens (capped by the model).
pub fn plan_with_ctx(
    hw: &HardwareProfile,
    files: &[RepoFile],
    shape: Option<&ModelShape>,
    wish: &Wish,
    ctx_tokens: u64,
    used_bytes: u64,
    reserve_bytes: u64,
) -> Result<Plan> {
    let mut cands = candidates(files);
    if let Some(file) = &wish.file {
        cands.retain(|c| {
            c.files
                .iter()
                .any(|f| f.path == *file || f.path.ends_with(&format!("/{file}")))
        });
        if cands.is_empty() {
            return Err(Error::not_found(format!(
                "file '{file}' is not in this repository"
            )));
        }
    }
    if let Some(q) = &wish.quant {
        let want = q.to_ascii_uppercase();
        cands.retain(|c| {
            c.quant.as_ref().is_some_and(|cq| {
                cq.name == want
                    || cq.name.trim_start_matches("UD-") == want.trim_start_matches("UD-")
            })
        });
        if cands.is_empty() {
            return Err(Error::not_found(format!(
                "no {want} files in this repository"
            )));
        }
    }
    if cands.is_empty() {
        return Err(Error::not_found(
            "this repository contains no GGUF model files",
        ));
    }

    let ram_budget = hw.total_ram_bytes.saturating_sub(reserve_bytes);
    let device_budget = match (hw.gpu, hw.gpu_memory_bytes) {
        (Gpu::Metal | Gpu::Cuda, Some(g)) => g.min(ram_budget),
        _ => ram_budget,
    };
    let available = device_budget.saturating_sub(used_bytes);
    let max_ctx = shape.and_then(|s| s.context_length).unwrap_or(u64::MAX);
    let ctx = ctx_tokens.min(max_ctx).max(512);

    let estimate = |c: &Candidate| -> (u64, u64) {
        let kv = kv_bytes_per_token(shape, c.size) * ctx;
        let overhead = GIB / 2 + c.size / 20;
        (kv, c.size + kv + overhead)
    };

    // Highest quality first; "too good to be useful" quantizations last.
    cands.sort_by(|a, b| {
        let score = |c: &Candidate| {
            let bits = c.quant.as_ref().map_or(8.0, |q| q.bits);
            if bits > MAX_USEFUL_BITS { -bits } else { bits }
        };
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let pick = |pred: &dyn Fn(u64) -> bool| cands.iter().find(|c| pred(estimate(c).1)).cloned();
    let comfortable = |need: u64| need <= available.saturating_mul(85) / 100;
    let (chosen, fit) = if let Some(c) = pick(&comfortable) {
        (c, Fit::Fits)
    } else if let Some(c) = pick(&|need| need <= available) {
        (c, Fit::Tight)
    } else {
        let smallest = cands
            .iter()
            .min_by_key(|c| c.size)
            .cloned()
            .expect("non-empty");
        (smallest, Fit::DoesNotFit)
    };
    let (kv, need) = estimate(&chosen);
    let gb = |b: u64| format!("{:.1} GB", b as f64 / GIB as f64);
    let qname = chosen.quant.as_ref().map(|q| q.name.clone());
    let reason = match fit {
        Fit::Fits => format!(
            "{} needs about {} of {} available – fits comfortably",
            qname.as_deref().unwrap_or("this model"),
            gb(need),
            gb(available)
        ),
        Fit::Tight => format!(
            "{} needs about {} of {} available – fits, but leaves little room for other apps",
            qname.as_deref().unwrap_or("this model"),
            gb(need),
            gb(available)
        ),
        Fit::DoesNotFit => {
            let hint = if wish.context != ContextSize::Small {
                " Try a smaller context or a smaller model."
            } else {
                " Choose a smaller model."
            };
            let others = if used_bytes > 0 {
                format!(" ({} are used by other loaded models)", gb(used_bytes))
            } else {
                String::new()
            };
            format!(
                "even the smallest variant needs about {}, but only {} are available{}.{}",
                gb(need),
                gb(available),
                others,
                hint
            )
        }
    };
    Ok(Plan {
        size_bytes: chosen.size,
        quant: qname,
        files: chosen.files,
        ctx_tokens: ctx,
        gpu_layers: if hw.gpu == Gpu::None { 0 } else { -1 },
        threads: hw.performance_cores.max(1),
        kv_cache_bytes: kv,
        expected_ram_bytes: need,
        available_bytes: available,
        fit,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn f(path: &str, gb: f64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: (gb * 1e9) as u64,
            sha256: None,
        }
    }

    /// A Qwen3.6-35B-A3B-like repository.
    fn qwen35b() -> Vec<RepoFile> {
        vec![
            f("Qwen3.6-35B-A3B-UD-Q4_K_M.gguf", 22.1),
            f("Qwen3.6-35B-A3B-UD-Q5_K_M.gguf", 26.5),
            f("Qwen3.6-35B-A3B-UD-Q6_K.gguf", 29.3),
            f("Qwen3.6-35B-A3B-Q8_0.gguf", 36.9),
            f("BF16/Qwen3.6-35B-A3B-BF16-00001-of-00002.gguf", 35.0),
            f("BF16/Qwen3.6-35B-A3B-BF16-00002-of-00002.gguf", 34.4),
            f("mmproj-F16.gguf", 0.9),
            f("README.md", 0.0),
        ]
    }

    fn qwen4b() -> Vec<RepoFile> {
        vec![
            f("Qwen3.5-4B-Q4_K_M.gguf", 2.7),
            f("Qwen3.5-4B-Q8_0.gguf", 4.5),
            f("Qwen3.5-4B-BF16.gguf", 8.4),
        ]
    }

    fn shape() -> ModelShape {
        ModelShape {
            context_length: Some(262_144),
            block_count: Some(40),
            head_count_kv: Some(2),
            head_dim: Some(256),
        }
    }

    fn wish(ctx: ContextSize) -> Wish {
        Wish {
            context: ctx,
            ..Default::default()
        }
    }

    // covers: M1-AC-02
    #[test]
    fn plans_sensibly_across_hardware() {
        struct Case {
            ram: u64,
            repo: fn() -> Vec<RepoFile>,
            ctx: ContextSize,
            used_gb: u64,
            quant: Option<&'static str>,
            fit: Fit,
        }
        let cases = [
            // 128 GB: best useful quality (Q8_0), not BF16.
            Case {
                ram: 128,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("Q8_0"),
                fit: Fit::Fits,
            },
            Case {
                ram: 128,
                repo: qwen35b,
                ctx: ContextSize::Large,
                used_gb: 0,
                quant: Some("Q8_0"),
                fit: Fit::Fits,
            },
            // 64 GB (48 GB GPU): Q8_0 + KV still fits.
            Case {
                ram: 64,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("Q8_0"),
                fit: Fit::Fits,
            },
            // 48 GB (36 GB GPU): drops to a 5/4-bit variant.
            Case {
                ram: 48,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("UD-Q5_K_M"),
                fit: Fit::Fits,
            },
            // 36 GB (24 GiB GPU): Q4 plus a 32k KV cache no longer fits …
            Case {
                ram: 36,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("UD-Q4_K_M"),
                fit: Fit::DoesNotFit,
            },
            // … with a small context it fits, but tightly.
            Case {
                ram: 36,
                repo: qwen35b,
                ctx: ContextSize::Small,
                used_gb: 0,
                quant: Some("UD-Q4_K_M"),
                fit: Fit::Tight,
            },
            // 16 GB: does not fit – honest answer.
            Case {
                ram: 16,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("UD-Q4_K_M"),
                fit: Fit::DoesNotFit,
            },
            // 128 GB but 80 GB used by other models.
            Case {
                ram: 128,
                repo: qwen35b,
                ctx: ContextSize::Medium,
                used_gb: 80,
                quant: Some("UD-Q4_K_M"),
                fit: Fit::DoesNotFit,
            },
            // Small model: Q8_0 (not BF16) even with lots of RAM.
            Case {
                ram: 128,
                repo: qwen4b,
                ctx: ContextSize::Medium,
                used_gb: 0,
                quant: Some("Q8_0"),
                fit: Fit::Fits,
            },
            // 8 GB: a 4B model at Q4 fits, but tightly.
            Case {
                ram: 8,
                repo: qwen4b,
                ctx: ContextSize::Small,
                used_gb: 0,
                quant: Some("Q4_K_M"),
                fit: Fit::Tight,
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            let hw = HardwareProfile::apple(c.ram);
            let p = plan(
                &hw,
                &(c.repo)(),
                Some(&shape()),
                &wish(c.ctx),
                c.used_gb * GIB,
                4 * GIB,
            )
            .unwrap();
            assert_eq!(p.quant.as_deref(), c.quant, "case {i}: {}", p.reason);
            assert_eq!(p.fit, c.fit, "case {i}: {}", p.reason);
            assert!(!p.reason.is_empty());
            assert_eq!(p.gpu_layers, -1);
        }
    }

    // covers: M1-AC-02
    #[test]
    fn respects_wishes_and_model_limits() {
        let hw = HardwareProfile::apple(128);
        let w = Wish {
            quant: Some("Q4_K_M".into()),
            ..Default::default()
        };
        let p = plan(&hw, &qwen35b(), Some(&shape()), &w, 0, 4 * GIB).unwrap();
        assert_eq!(p.quant.as_deref(), Some("UD-Q4_K_M"));
        let w = Wish {
            file: Some("Qwen3.6-35B-A3B-UD-Q6_K.gguf".into()),
            ..Default::default()
        };
        assert_eq!(
            plan(&hw, &qwen35b(), None, &w, 0, 0)
                .unwrap()
                .quant
                .as_deref(),
            Some("UD-Q6_K")
        );
        let small_ctx = ModelShape {
            context_length: Some(4096),
            ..shape()
        };
        let p = plan(
            &hw,
            &qwen4b(),
            Some(&small_ctx),
            &wish(ContextSize::Large),
            0,
            0,
        )
        .unwrap();
        assert_eq!(p.ctx_tokens, 4096);
        let err = plan(
            &hw,
            &qwen35b(),
            None,
            &Wish {
                quant: Some("IQ1_S".into()),
                ..Default::default()
            },
            0,
            0,
        );
        assert_eq!(err.unwrap_err().code(), "not_found");
        assert_eq!(
            plan(&hw, &[f("README.md", 0.0)], None, &Wish::default(), 0, 0)
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[test]
    fn groups_split_files_and_ignores_incomplete_sets() {
        let files = vec![
            f("M-Q8_0-00001-of-00002.gguf", 10.0),
            f("M-Q8_0-00002-of-00002.gguf", 10.0),
            f("M-Q4_K_M-00001-of-00002.gguf", 6.0),
        ];
        let c = candidates(&files);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].files.len(), 2);
        assert_eq!(c[0].size, 20_000_000_000);
    }

    fn arb_files() -> impl Strategy<Value = Vec<RepoFile>> {
        let quants = prop::sample::select(vec![
            "Q2_K", "Q3_K_M", "Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "BF16",
        ]);
        prop::collection::vec((quants, 0.2f64..120.0), 1..6).prop_map(|v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (q, gb))| f(&format!("m{i}-{q}.gguf"), gb))
                .collect()
        })
    }

    proptest! {
        // covers: M1-AC-02
        #[test]
        fn never_plans_more_than_available_unless_it_says_so(
            ram in 4u64..256,
            used in 0u64..64,
            files in arb_files(),
            ctx in prop::sample::select(vec![ContextSize::Small, ContextSize::Medium, ContextSize::Large]),
        ) {
            let hw = HardwareProfile::apple(ram);
            let p = plan(&hw, &files, None, &wish(ctx), used * GIB, 4 * GIB).unwrap();
            match p.fit {
                Fit::Fits | Fit::Tight => prop_assert!(p.expected_ram_bytes <= p.available_bytes),
                Fit::DoesNotFit => prop_assert!(p.expected_ram_bytes > p.available_bytes),
            }
            prop_assert!(p.available_bytes <= hw.gpu_memory_bytes.unwrap());
        }
    }
}
