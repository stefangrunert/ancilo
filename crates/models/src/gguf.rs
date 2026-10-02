//! Reads the metadata header of GGUF files (without loading tensors).

use std::collections::BTreeMap;
use std::io::{BufReader, Read};
use std::path::Path;

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The metadata Ancilo cares about.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GgufMeta {
    pub architecture: Option<String>,
    pub name: Option<String>,
    pub context_length: Option<u64>,
    pub block_count: Option<u64>,
    pub embedding_length: Option<u64>,
    pub head_count: Option<u64>,
    pub head_count_kv: Option<u64>,
    pub key_length: Option<u64>,
    pub value_length: Option<u64>,
    pub expert_count: Option<u64>,
    pub has_chat_template: bool,
    /// The chat template mentions tools – a hint that tool calling works.
    pub template_supports_tools: bool,
    pub pooling_type: Option<u64>,
}

#[derive(Debug, Clone)]
enum Val {
    Uint(u64),
    Int(i64),
    Float(f64),
    Str(String),
    Skipped,
}

struct Reader<R: Read> {
    inner: R,
}

impl<R: Read> Reader<R> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.inner
            .read_exact(&mut b)
            .map_err(|e| Error::invalid(format!("truncated GGUF header: {e}")))?;
        Ok(b)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    fn skip(&mut self, n: u64) -> Result<()> {
        let copied = std::io::copy(&mut (&mut self.inner).take(n), &mut std::io::sink())?;
        if copied != n {
            return Err(Error::invalid("truncated GGUF header"));
        }
        Ok(())
    }
    fn string(&mut self, keep: bool) -> Result<Option<String>> {
        let len = self.u64()?;
        if len > 64 * 1024 * 1024 {
            return Err(Error::invalid("GGUF string too long"));
        }
        if !keep {
            self.skip(len)?;
            return Ok(None);
        }
        let mut buf = vec![0u8; len as usize];
        self.inner
            .read_exact(&mut buf)
            .map_err(|e| Error::invalid(format!("truncated GGUF header: {e}")))?;
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }
    fn value(&mut self, ty: u32, keep: bool) -> Result<Val> {
        Ok(match ty {
            0 => Val::Uint(u64::from(self.bytes::<1>()?[0])),
            1 => Val::Int(i64::from(self.bytes::<1>()?[0] as i8)),
            2 => Val::Uint(u64::from(u16::from_le_bytes(self.bytes()?))),
            3 => Val::Int(i64::from(i16::from_le_bytes(self.bytes()?))),
            4 => Val::Uint(u64::from(self.u32()?)),
            5 => Val::Int(i64::from(i32::from_le_bytes(self.bytes()?))),
            6 => Val::Float(f64::from(f32::from_le_bytes(self.bytes()?))),
            7 => {
                self.bytes::<1>()?;
                Val::Skipped
            }
            8 => match self.string(keep)? {
                Some(s) => Val::Str(s),
                None => Val::Skipped,
            },
            9 => {
                let elem = self.u32()?;
                let n = self.u64()?;
                let fixed = match elem {
                    0 | 1 | 7 => Some(1),
                    2 | 3 => Some(2),
                    4..=6 => Some(4),
                    10..=12 => Some(8),
                    _ => None,
                };
                match fixed {
                    Some(size) => self.skip(n * size)?,
                    None => {
                        for _ in 0..n {
                            self.value(elem, false)?;
                        }
                    }
                }
                Val::Skipped
            }
            10 => Val::Uint(self.u64()?),
            11 => Val::Int(i64::from_le_bytes(self.bytes()?)),
            12 => Val::Float(f64::from_le_bytes(self.bytes()?)),
            other => return Err(Error::invalid(format!("unknown GGUF value type {other}"))),
        })
    }
}

/// Reads metadata from any reader positioned at the start of a GGUF file.
pub fn read_meta(r: impl Read) -> Result<GgufMeta> {
    let mut r = Reader { inner: r };
    if &r.bytes::<4>()? != b"GGUF" {
        return Err(Error::invalid("not a GGUF file (wrong magic)"));
    }
    let version = r.u32()?;
    if !(2..=3).contains(&version) {
        return Err(Error::invalid(format!(
            "unsupported GGUF version {version}"
        )));
    }
    let _tensors = r.u64()?;
    let kv_count = r.u64()?;
    let mut kv: BTreeMap<String, Val> = BTreeMap::new();
    for _ in 0..kv_count.min(100_000) {
        let key = r.string(true)?.unwrap_or_default();
        let ty = r.u32()?;
        // Keep only small strings we need; tokenizer arrays are skipped.
        let keep = key == "general.architecture"
            || key == "general.name"
            || key == "tokenizer.chat_template";
        let v = r.value(ty, keep)?;
        kv.insert(key, v);
    }
    let arch = match kv.get("general.architecture") {
        Some(Val::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let num = |key: &str| -> Option<u64> {
        match kv.get(key)? {
            Val::Uint(v) => Some(*v),
            Val::Int(v) => u64::try_from(*v).ok(),
            Val::Float(v) => Some(*v as u64),
            _ => None,
        }
    };
    let arch_num = |suffix: &str| arch.as_ref().and_then(|a| num(&format!("{a}.{suffix}")));
    let template = match kv.get("tokenizer.chat_template") {
        Some(Val::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    Ok(GgufMeta {
        name: match kv.get("general.name") {
            Some(Val::Str(s)) => Some(s.clone()),
            _ => None,
        },
        context_length: arch_num("context_length"),
        block_count: arch_num("block_count"),
        embedding_length: arch_num("embedding_length"),
        head_count: arch_num("attention.head_count"),
        head_count_kv: arch_num("attention.head_count_kv"),
        key_length: arch_num("attention.key_length"),
        value_length: arch_num("attention.value_length"),
        expert_count: arch_num("expert_count"),
        has_chat_template: template.is_some(),
        template_supports_tools: template.is_some_and(|t| t.contains("tools")),
        pooling_type: arch_num("pooling_type"),
        architecture: arch,
    })
}

pub fn read_meta_file(path: &Path) -> Result<GgufMeta> {
    let f = std::fs::File::open(path)
        .map_err(|e| Error::not_found(format!("cannot open {}: {e}", path.display())))?;
    read_meta(BufReader::with_capacity(1 << 20, f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_testkit::gguf::fake_gguf;

    #[test]
    fn reads_fake_gguf() {
        let bytes = fake_gguf("qwen3", "Test", 32768, 10_000);
        let m = read_meta(bytes.as_slice()).unwrap();
        assert_eq!(m.architecture.as_deref(), Some("qwen3"));
        assert_eq!(m.context_length, Some(32768));
        assert_eq!(m.block_count, Some(48));
        assert!(m.has_chat_template);
    }

    #[test]
    fn rejects_non_gguf() {
        assert_eq!(
            read_meta(&b"PK\x03\x04garbage"[..]).unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(read_meta(&b"GGUF"[..]).unwrap_err().code(), "invalid_input");
    }
}
