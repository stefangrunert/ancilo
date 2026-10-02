//! Writes minimal, valid GGUF (v3) files: header plus metadata, padded with
//! deterministic bytes to a realistic size. Enough for Ancilo's GGUF metadata
//! reader and for fake model servers; real llama.cpp cannot load them.

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// Metadata values supported by the writer.
pub enum Kv<'a> {
    Str(&'a str),
    U32(u32),
    U64(u64),
    F32(f32),
    Bool(bool),
    StrArray(&'a [&'a str]),
}

pub fn write_gguf(kvs: &[(&str, Kv<'_>)], pad_to: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"GGUF");
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(&0u64.to_le_bytes()); // tensor count
    buf.extend_from_slice(&(kvs.len() as u64).to_le_bytes());
    for (key, value) in kvs {
        put_str(&mut buf, key);
        match value {
            Kv::Str(s) => {
                buf.extend_from_slice(&8u32.to_le_bytes());
                put_str(&mut buf, s);
            }
            Kv::U32(v) => {
                buf.extend_from_slice(&4u32.to_le_bytes());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Kv::U64(v) => {
                buf.extend_from_slice(&10u32.to_le_bytes());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Kv::F32(v) => {
                buf.extend_from_slice(&6u32.to_le_bytes());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Kv::Bool(v) => {
                buf.extend_from_slice(&7u32.to_le_bytes());
                buf.push(u8::from(*v));
            }
            Kv::StrArray(items) => {
                buf.extend_from_slice(&9u32.to_le_bytes());
                buf.extend_from_slice(&8u32.to_le_bytes());
                buf.extend_from_slice(&(items.len() as u64).to_le_bytes());
                for s in *items {
                    put_str(&mut buf, s);
                }
            }
        }
    }
    let mut x: u32 = 0x9e37_79b9;
    while buf.len() < pad_to {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        buf.push((x & 0xff) as u8);
    }
    buf
}

/// A typical model file: architecture, name, context length, block count and a
/// small tokenizer array (exercises array skipping in readers).
pub fn fake_gguf(arch: &str, name: &str, context_length: u64, size: usize) -> Vec<u8> {
    let ctx_key = format!("{arch}.context_length");
    let blocks_key = format!("{arch}.block_count");
    write_gguf(
        &[
            ("general.architecture", Kv::Str(arch)),
            ("general.name", Kv::Str(name)),
            ("general.file_type", Kv::U32(15)),
            (&ctx_key, Kv::U64(context_length)),
            (&blocks_key, Kv::U32(48)),
            (
                "tokenizer.ggml.tokens",
                Kv::StrArray(&["<s>", "</s>", "a", "b"]),
            ),
            (
                "tokenizer.chat_template",
                Kv::Str("{% for m in messages %}{{m.content}}{% endfor %}"),
            ),
            ("general.quantized", Kv::Bool(true)),
            ("general.scale", Kv::F32(1.0)),
        ],
        size,
    )
}
