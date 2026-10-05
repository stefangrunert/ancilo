//! The bundled model list against the real Hugging Face: every address
//! exists, its files have the listed sizes, and llama.cpp knows the
//! architecture. Network – runs with the real tests (`just test-real`).

use ancilo_models::catalog::Catalog;
use ancilo_models::hf::HfClient;

/// Architectures the pinned llama.cpp build (b11270) supports, as far as the
/// catalog uses them.
const SUPPORTED: &[&str] = &[
    "qwen3",
    "qwen35",
    "qwen35moe",
    "qwen3moe",
    "qwen3next",
    "gemma4",
    "gpt-oss",
    "bert",
];

#[tokio::test]
#[ignore = "network: checks the catalog against huggingface.co"]
async fn the_catalog_matches_hugging_face() {
    let hf = HfClient::new("https://huggingface.co", None, &Default::default());
    let mut problems = Vec::new();
    for m in Catalog::bundled().models {
        let repo = m.address.trim_start_matches("hf.co/");
        let info = match hf.model_info(repo).await {
            Ok(i) => i,
            Err(e) => {
                problems.push(format!("{}: {}", m.id, e.message()));
                continue;
            }
        };
        if let Some(arch) = &info.architecture
            && !SUPPORTED.contains(&arch.as_str())
        {
            problems.push(format!(
                "{}: architecture {arch} not known to be supported",
                m.id
            ));
        }
        let files = hf.files(repo, "main").await.unwrap_or_default();
        for (quant, size) in &m.sizes {
            let actual: u64 = files
                .iter()
                .filter(|f| {
                    !f.path.contains('/')
                        && !f.path.to_lowercase().contains("mmproj")
                        && f.path.to_uppercase().contains(&format!("-{quant}"))
                })
                .map(|f| f.size)
                .sum();
            let off = (actual as f64 - *size as f64).abs() / *size as f64;
            if actual == 0 || off > 0.05 {
                problems.push(format!("{} {quant}: listed {size}, found {actual}", m.id));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
