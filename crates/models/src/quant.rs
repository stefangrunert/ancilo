//! Quantization names in GGUF file names and what they mean for quality and size.

/// A recognised quantization.
#[derive(Debug, Clone, PartialEq)]
pub struct Quant {
    /// Canonical name, e.g. `Q4_K_M`, `UD-Q6_K_XL`, `BF16`.
    pub name: String,
    /// Approximate bits per weight – the quality/size measure used by the planner.
    pub bits: f64,
}

const KNOWN: &[(&str, f64)] = &[
    ("F32", 32.0),
    ("BF16", 16.0),
    ("F16", 16.0),
    ("Q8_K_XL", 8.8),
    ("Q8_0", 8.5),
    ("Q6_K_XL", 7.0),
    ("Q6_K_L", 6.8),
    ("Q6_K_M", 6.7),
    ("Q6_K", 6.56),
    ("Q5_K_XL", 5.9),
    ("Q5_K_L", 5.8),
    ("Q5_K_M", 5.69),
    ("Q5_K_S", 5.54),
    ("Q5_1", 6.0),
    ("Q5_0", 5.5),
    ("MXFP4", 4.25),
    ("NVFP4", 4.5),
    ("Q4_K_XL", 5.0),
    ("Q4_K_L", 4.9),
    ("Q4_K_M", 4.85),
    ("Q4_K_S", 4.58),
    ("Q4_1", 5.0),
    ("Q4_0", 4.5),
    ("IQ4_NL", 4.5),
    ("IQ4_XS", 4.25),
    ("Q3_K_XL", 4.1),
    ("Q3_K_L", 4.27),
    ("Q3_K_M", 3.91),
    ("Q3_K_S", 3.5),
    ("IQ3_M", 3.66),
    ("IQ3_S", 3.44),
    ("IQ3_XS", 3.3),
    ("IQ3_XXS", 3.06),
    ("Q2_K_XL", 3.2),
    ("Q2_K_L", 3.1),
    ("Q2_K", 2.96),
    ("IQ2_M", 2.7),
    ("IQ2_S", 2.5),
    ("IQ2_XS", 2.31),
    ("IQ2_XXS", 2.06),
    ("IQ1_M", 1.75),
    ("IQ1_S", 1.56),
];

/// Extracts the quantization from a file name such as
/// `Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf` or `model.Q8_0.gguf`.
pub fn from_file_name(name: &str) -> Option<Quant> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let upper = base.to_ascii_uppercase();
    let stem = upper.trim_end_matches(".GGUF");
    // Split parts like "-00001-OF-00003" are not quantizations.
    let tokens: Vec<&str> = stem.split(['-', '.', '_']).collect();
    // Try the longest matching known name ending at some token boundary.
    let mut best: Option<Quant> = None;
    for (known, bits) in KNOWN {
        let parts: Vec<&str> = known.split('_').collect();
        let n = parts.len();
        let found = tokens.windows(n).any(|w| w == parts.as_slice());
        if found && best.as_ref().is_none_or(|b| known.len() > b.name.len()) {
            best = Some(Quant {
                name: (*known).to_string(),
                bits: *bits,
            });
        }
    }
    let mut q = best?;
    if tokens
        .windows(2)
        .any(|w| w == ["UD", q.name.split('_').next().unwrap_or("")])
        || stem.contains(&format!("UD-{}", q.name))
    {
        q.name = format!("UD-{}", q.name);
    }
    Some(q)
}

/// Files that are not language models themselves.
pub fn is_auxiliary(name: &str) -> bool {
    let lower = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    lower.starts_with("mmproj")
        || lower.contains("imatrix")
        || lower.contains("-mmproj")
        || !lower.ends_with(".gguf")
}

/// `(part, total)` for split files like `name-00002-of-00003.gguf`.
pub fn split_part(name: &str) -> Option<(u32, u32)> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".gguf")?;
    let idx = stem.rfind("-of-")?;
    let total: u32 = stem[idx + 4..].parse().ok()?;
    let before = &stem[..idx];
    let dash = before.rfind('-')?;
    let part: u32 = before[dash + 1..].parse().ok()?;
    Some((part, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_names() {
        let cases = [
            ("Qwen3.6-35B-A3B-Q8_0.gguf", "Q8_0"),
            ("Qwen3.6-35B-A3B-UD-Q4_K_M.gguf", "UD-Q4_K_M"),
            ("Qwen3.8-27B-UD-Q6_K_XL.gguf", "UD-Q6_K_XL"),
            ("model.Q4_K_M.gguf", "Q4_K_M"),
            ("Qwen3.5-4B-BF16.gguf", "BF16"),
            ("gemma-IQ4_XS.gguf", "IQ4_XS"),
            ("Q8_0/Model-Q8_0-00001-of-00002.gguf", "Q8_0"),
            ("gpt-oss-120b-MXFP4.gguf", "MXFP4"),
        ];
        for (file, expected) in cases {
            assert_eq!(from_file_name(file).unwrap().name, expected, "{file}");
        }
        assert!(from_file_name("model.gguf").is_none());
        assert!(from_file_name("Q4_K_M.gguf").unwrap().bits > 4.0);
    }

    #[test]
    fn recognises_auxiliary_and_split_files() {
        assert!(is_auxiliary("mmproj-F16.gguf"));
        assert!(is_auxiliary("imatrix_unsloth.gguf"));
        assert!(is_auxiliary("README.md"));
        assert!(!is_auxiliary("Qwen-Q8_0.gguf"));
        assert_eq!(split_part("M-Q8_0-00002-of-00003.gguf"), Some((2, 3)));
        assert_eq!(split_part("M-Q8_0.gguf"), None);
    }
}
