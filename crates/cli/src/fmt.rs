//! Human-readable output. Pure functions – snapshot-tested.

use serde_json::Value;

pub fn gb(bytes: f64) -> String {
    if bytes >= 1e9 {
        format!("{:.1} GB", bytes / 1e9)
    } else {
        format!("{:.0} MB", bytes / 1e6)
    }
}

/// Memory in binary units ("128 GB" for 128 GiB of RAM).
pub fn mem(bytes: f64) -> String {
    let gib = bytes / (1u64 << 30) as f64;
    if gib >= 10.0 && (gib - gib.round()).abs() < 0.05 {
        format!("{:.0} GB", gib)
    } else {
        format!("{gib:.1} GB")
    }
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

pub fn ctx(tokens: &Value) -> String {
    let t = tokens.as_u64().unwrap_or(0);
    if t >= 1024 {
        format!("{}k", t / 1024)
    } else {
        t.to_string()
    }
}

fn status_word(v: &Value) -> String {
    let s = v["status"].as_str().unwrap_or("?");
    match s {
        "running" => match v["instance"]["tokens_per_sec"].as_f64() {
            Some(t) => format!("running · {t:.0} tok/s"),
            None => "running".into(),
        },
        "downloading" => match v["download"]["percent"].as_f64() {
            Some(p) => format!("downloading {p:.0} %"),
            None => "downloading".into(),
        },
        "download_failed" => "download failed".into(),
        other => other.to_string(),
    }
}

/// `ancilo list`
pub fn model_table(models: &Value) -> String {
    let Some(list) = models.as_array() else {
        return String::new();
    };
    if list.is_empty() {
        return "No models yet. Add one with:\n  ancilo add hf.co/unsloth/Qwen3.6-35B-A3B-GGUF\n"
            .into();
    }
    let rows: Vec<[String; 6]> = list
        .iter()
        .map(|m| {
            let roles: Vec<&str> = m["roles"]
                .as_array()
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            [
                m["id"].as_str().unwrap_or("").to_string(),
                status_word(m),
                m["quant"].as_str().unwrap_or("-").to_string(),
                gb(num(&m["size_bytes"])),
                ctx(&m["ctx_tokens"]),
                if roles.is_empty() {
                    "-".into()
                } else {
                    roles.join(",")
                },
            ]
        })
        .collect();
    let header = ["MODEL", "STATUS", "QUANT", "SIZE", "CONTEXT", "ROLES"];
    let mut widths = header.map(str::len);
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: &[String]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = line(&header.map(String::from));
    out.push('\n');
    for r in &rows {
        out.push_str(&line(r));
        out.push('\n');
    }
    out
}

/// `ancilo status <model>`
pub fn model_detail(m: &Value) -> String {
    let mut out = format!(
        "{} ({})\n",
        m["name"].as_str().unwrap_or(""),
        m["id"].as_str().unwrap_or("")
    );
    let mut row = |k: &str, v: String| out.push_str(&format!("  {k:<9} {v}\n"));
    row("status", status_word(m));
    row("quant", m["quant"].as_str().unwrap_or("-").to_string());
    row("size", gb(num(&m["size_bytes"])));
    row("context", ctx(&m["ctx_tokens"]));
    row(
        "memory",
        format!("about {}", mem(num(&m["expected_ram_bytes"]))),
    );
    row("source", m["source"].as_str().unwrap_or("").to_string());
    if let Some(p) = m["path"].as_str() {
        row("file", p.to_string());
    }
    if let Some(ms) = m["instance"]["load_ms"].as_u64() {
        row("loaded in", format!("{:.1} s", ms as f64 / 1000.0));
    }
    if let Some(e) = m["failure"].as_str() {
        row("problem", e.to_string());
    }
    out
}

/// The plan shown before adding a model.
pub fn plan_summary(p: &Value) -> String {
    let plan = &p["plan"];
    let file = plan["files"][0]["path"].as_str().unwrap_or("");
    let mark = match plan["fit"].as_str() {
        Some("fits") => "✓",
        Some("tight") => "!",
        _ => "✗",
    };
    let mut out = format!(
        "{file}\n  {} · {} · context {}\n  {mark} {}\n",
        plan["quant"].as_str().unwrap_or("-"),
        gb(num(&plan["size_bytes"])),
        ctx(&plan["ctx_tokens"]),
        plan["reason"].as_str().unwrap_or("")
    );
    match (p["existing"].as_str(), p["existing_in"].as_str()) {
        (Some(path), Some(tool)) => out.push_str(&format!(
            "  using the existing file from {} ({path})\n",
            tool.replace('_', " ")
        )),
        (Some(path), None) => out.push_str(&format!("  using {path}\n")),
        _ => out.push_str(&format!("  download: {}\n", gb(num(&p["download_bytes"])))),
    }
    out
}

/// A progress line from a `download.progress` event.
pub fn progress_line(data: &Value) -> String {
    let bytes = num(&data["bytes"]);
    let total = data["total"].as_f64();
    let rate = num(&data["bytes_per_sec"]);
    let mut s = match (data["percent"].as_f64(), total) {
        (Some(p), Some(t)) => format!("downloading {p:>3.0} %  {} of {}", gb(bytes), gb(t)),
        _ => format!("downloading {}", gb(bytes)),
    };
    if rate > 0.0 {
        s.push_str(&format!("  {}/s", gb(rate)));
        if let Some(t) = total {
            let secs = ((t - bytes) / rate).max(0.0) as u64;
            s.push_str(&format!("  ~{}", eta(secs)));
        }
    }
    s
}

fn eta(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

/// `ancilo hardware`
pub fn hardware(v: &Value) -> String {
    let hw = &v["hardware"];
    let gpu = match hw["gpu"].as_str() {
        Some("metal") => "Apple GPU (Metal, unified memory)",
        Some("cuda") => "NVIDIA GPU (CUDA)",
        _ => "none (CPU only)",
    };
    format!(
        "{}\n  memory    {} RAM\n  gpu       {gpu}\n  models    {} usable · {} in use · {} free\n  cores     {} performance / {} total\n  disk      {} free\n",
        hw["chip"].as_str().unwrap_or(""),
        mem(num(&hw["total_ram_bytes"])),
        mem(num(&v["model_budget_bytes"])),
        mem(num(&v["used_bytes"])),
        mem(num(&v["free_for_models_bytes"])),
        hw["performance_cores"],
        hw["total_cores"],
        gb(num(&hw["free_disk_bytes"])),
    )
}

/// `ancilo eval`
pub fn eval_report(r: &Value) -> String {
    let mut out = format!(
        "{} · {} · {}\n",
        r["suite"].as_str().unwrap_or(""),
        r["target"]["model"].as_str().unwrap_or(""),
        r["target"]["label"].as_str().unwrap_or("")
    );
    for t in r["tasks"].as_array().into_iter().flatten() {
        let runs = t["runs"].as_array().map_or(0, Vec::len);
        let passed = t["runs"]
            .as_array()
            .map_or(0, |r| r.iter().filter(|x| x["passed"] == true).count());
        let mark = if passed == runs {
            "✓"
        } else if passed == 0 {
            "✗"
        } else {
            "~"
        };
        out.push_str(&format!(
            "  {mark} {:<24} {passed}/{runs}",
            t["id"].as_str().unwrap_or("")
        ));
        if let Some(f) = t["runs"]
            .as_array()
            .and_then(|r| r.iter().find_map(|x| x["failure"].as_str()))
        {
            let f: String = f.chars().take(90).collect();
            out.push_str(&format!("  {f}"));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "success rate: {:.0} %\n",
        r["success_rate"].as_f64().unwrap_or(0.0) * 100.0
    ));
    out
}

/// `ancilo eval delegation`
pub fn delegation_report(r: &Value) -> String {
    let mut out = format!(
        "{} · {}\n",
        r["suite"].as_str().unwrap_or(""),
        r["model"].as_str().unwrap_or("")
    );
    for t in r["tasks"].as_array().into_iter().flatten() {
        let runs = t["runs"].as_array().map_or(0, Vec::len);
        let passed = t["runs"]
            .as_array()
            .map_or(0, |r| r.iter().filter(|x| x["passed"] == true).count());
        let mark = if passed == runs {
            "✓"
        } else if passed == 0 {
            "✗"
        } else {
            "~"
        };
        let secs = t["runs"][0]["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
        out.push_str(&format!(
            "  {mark} {:<24} {passed}/{runs}  {secs:>5.1} s",
            t["id"].as_str().unwrap_or("")
        ));
        if let Some(f) = t["runs"]
            .as_array()
            .and_then(|r| r.iter().find_map(|x| x["failure"].as_str()))
        {
            let f: String = f.chars().take(80).collect();
            out.push_str(&format!("  {f}"));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "success rate: {:.0} %\n",
        r["success_rate"].as_f64().unwrap_or(0.0) * 100.0
    ));
    out
}

/// Compact rendering of tool arguments for progress lines.
pub fn short_args(v: &Value) -> String {
    let s = match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    };
    let s = s.replace('\n', " ");
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}

/// `ancilo run`, `ancilo task`
pub fn task(v: &Value) -> String {
    let mut out = format!(
        "{} · {} · {}\n",
        v["task_id"].as_str().unwrap_or(""),
        v["status"].as_str().unwrap_or(""),
        v["model"].as_str().unwrap_or("")
    );
    if let Some(s) = v["summary"].as_str() {
        out.push_str(&format!("\n{}\n", s.trim()));
    }
    if let Some(files) = v["changed_files"].as_array().filter(|f| !f.is_empty()) {
        out.push('\n');
        for f in files {
            out.push_str(&format!(
                "  {:<8} {}  +{} −{}\n",
                f["kind"].as_str().unwrap_or(""),
                f["path"].as_str().unwrap_or(""),
                f["added"],
                f["removed"]
            ));
        }
    }
    if let Some(b) = v["branch"].as_str() {
        out.push_str(&format!("\nbranch: {b}\n"));
    }
    if let Some(n) = v["next_step"].as_str() {
        out.push_str(&format!("next: {n}\n"));
    }
    if let Some(d) = v["diff"].as_str() {
        out.push_str(&format!("\n{d}\n"));
    }
    out
}

/// `ancilo tasks`
pub fn task_table(v: &Value) -> String {
    let Some(list) = v.as_array().filter(|l| !l.is_empty()) else {
        return "No tasks yet. Delegate one with: ancilo run \"write tests for src/parser.rs\"\n"
            .into();
    };
    let mut out = String::new();
    for t in list {
        let summary: String = t["summary"]
            .as_str()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        out.push_str(&format!(
            "{:<16} {:<9} {:<20} {}\n",
            t["task_id"].as_str().unwrap_or(""),
            t["status"].as_str().unwrap_or(""),
            t["diff_stat"].as_str().unwrap_or("-"),
            summary
        ));
    }
    out
}

/// `ancilo connect` / `disconnect`
pub fn connect(v: &Value) -> String {
    let mut out = String::new();
    for a in v["actions"].as_array().into_iter().flatten() {
        out.push_str(&format!("  ✓ {}\n", a.as_str().unwrap_or("")));
    }
    if v["connected"] == true {
        out.push_str(if v["verified"] == true {
            "connected and verified (MCP handshake ok)\n"
        } else {
            "connected, but the MCP check failed – see `ancilo daemon status`\n"
        });
    }
    if let Some(n) = v["notes"].as_str().filter(|n| !n.is_empty()) {
        out.push_str(&format!("{n}\n"));
    }
    out
}

/// Error line for users, with a hint that matches the cause.
pub fn error(code: &str, message: &str) -> String {
    let lower = message.to_lowercase();
    let hint = match code {
        "insufficient_resources" if lower.contains("disk") => {
            "\n  hint: free some disk space, or pick a smaller quantization, e.g. `--quant Q4_K_M`"
        }
        "insufficient_resources" if lower.contains("stop another model") => {
            "\n  hint: `ancilo list` shows loaded models; `ancilo stop <model>` frees memory"
        }
        "insufficient_resources" => {
            "\n  hint: try `--context small`, a smaller quantization (`--quant Q4_K_M`) or a smaller model"
        }
        "unavailable" => {
            "\n  hint: check your internet connection; `ancilo daemon status` shows the daemon"
        }
        _ => "",
    };
    format!("error: {message}{hint}")
}

fn pct(v: &Value) -> String {
    v.as_f64()
        .map_or("–".into(), |x| format!("{:.0} %", x * 100.0))
}

fn secs(v: &Value) -> String {
    v.as_f64()
        .map_or("–".into(), |ms| format!("{:.1} s", ms / 1000.0))
}

/// A success rate with its interval: `80 % (49–94 %, n=10)`
fn rate(r: &Value) -> String {
    if r.is_null() {
        return "–".into();
    }
    format!(
        "{} ({:.0}–{:.0} %, n={})",
        pct(&r["rate"]),
        r["low"].as_f64().unwrap_or(0.0) * 100.0,
        r["high"].as_f64().unwrap_or(0.0) * 100.0,
        r["n"]
    )
}

/// `ancilo route`
pub fn routes(v: &Value) -> String {
    if let Some(model) = v["model"].as_str() {
        return format!(
            "{model} (via {}{})\n",
            v["via"].as_str().unwrap_or(""),
            v["kind"]
                .as_str()
                .map(|k| format!(", kind {k}"))
                .unwrap_or_default()
        );
    }
    let rules = v.as_array().cloned().unwrap_or_default();
    if rules.is_empty() {
        return "no rules – every task kind uses the role (delegation) or the default model\n"
            .into();
    }
    rules
        .iter()
        .map(|r| {
            format!(
                "{:<9} → {}\n",
                r["kind"].as_str().unwrap_or(""),
                r["model"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// `ancilo compare`, `ancilo comparisons <id>`
pub fn comparison(v: &Value) -> String {
    let mut out = format!(
        "{} · {} · {} · kind {}{}\n",
        v["id"].as_str().unwrap_or(""),
        v["status"].as_str().unwrap_or(""),
        v["progress"].as_str().unwrap_or(""),
        v["kind"].as_str().unwrap_or(""),
        if v["kind_estimated"] == true {
            " (estimated)"
        } else {
            ""
        }
    );
    if v["blind"] == true && v["revealed"] != true {
        out.push_str("blind: models are revealed after `ancilo rate <id> <label>`\n");
    }
    out.push_str(&format!(
        "  {:<4} {:<26} {:<24} {:>8} {:>8} {:>7} {:>6}  diff\n",
        "", "model", "success", "p50", "load", "tok/s", "steps"
    ));
    for a in v["ranking"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {:<4} {:<26} {:<24} {:>8} {:>8} {:>7} {:>6}  {}\n",
            a["label"].as_str().unwrap_or(""),
            a["model"].as_str().unwrap_or("?"),
            rate(&a["success"]),
            secs(&a["duration_p50_ms"]),
            secs(&a["load_ms"]),
            a["tokens_per_s"]
                .as_f64()
                .map_or("–".into(), |t| format!("{t:.0}")),
            a["steps_mean"]
                .as_f64()
                .map_or("–".into(), |t| format!("{t:.1}")),
            a["typical_diff"].as_str().unwrap_or("–"),
        ));
    }
    if let Some(verdict) = v["verdict"].as_str() {
        out.push_str(&format!("{verdict}\n"));
    }
    for s in v["judgement"]["scores"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  judge ({}, model-based): {} {} – {}\n",
            v["judgement"]["judge_model"].as_str().unwrap_or(""),
            s["label"].as_str().unwrap_or(""),
            s["score"],
            s["reason"].as_str().unwrap_or("")
        ));
    }
    let branches: Vec<&str> = v["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["branch"].as_str())
        .collect();
    if let Some(b) = branches.first() {
        out.push_str(&format!(
            "results on branches like {b} – inspect with `git diff {}...{b}`\n",
            v["config"]["base_commit"]
                .as_str()
                .map(|c| &c[..c.len().min(10)])
                .unwrap_or("HEAD")
        ));
    }
    if let Some(e) = v["error"].as_str() {
        out.push_str(&format!("error: {e}\n"));
    }
    out
}

pub fn comparison_list(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "no comparisons yet – `ancilo compare \"<task>\" -m a,b`\n".into();
    }
    list.iter()
        .map(|c| {
            let leader = &c["ranking"][0];
            format!(
                "{:<13} {:<9} {:<8} {:<40} lead: {} {}\n",
                c["id"].as_str().unwrap_or(""),
                c["status"].as_str().unwrap_or(""),
                c["mode"].as_str().unwrap_or(""),
                c["title"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(40)
                    .collect::<String>(),
                leader["model"]
                    .as_str()
                    .or(leader["label"].as_str())
                    .unwrap_or("–"),
                pct(&leader["success"]["rate"]),
            )
        })
        .collect()
}

pub fn suites(v: &Value) -> String {
    if v["name"].is_string() {
        return format!(
            "saved suite {} ({} tasks)\n",
            v["name"].as_str().unwrap_or(""),
            v["tasks"]
        );
    }
    v.as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            format!(
                "{:<20} {:>3} tasks{}\n",
                s["name"].as_str().unwrap_or(""),
                s["tasks"],
                if s["builtin"] == true {
                    "  (built-in)"
                } else {
                    ""
                }
            )
        })
        .collect()
}

/// `ancilo ab status`, `start`, `stop`
pub fn ab_tests(v: &Value) -> String {
    let list = match v {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    if list.is_empty() {
        return "no A/B tests – `ancilo ab start <role> --b <model> --share 20%`\n".into();
    }
    list.iter()
        .map(|t| {
            format!(
                "{:<14} {:<11} A {} · B {} ({:.0} %){}  {}\n",
                t["id"].as_str().unwrap_or(""),
                t["role"].as_str().unwrap_or(""),
                t["model_a"].as_str().unwrap_or(""),
                t["model_b"].as_str().unwrap_or(""),
                t["share"].as_f64().unwrap_or(0.0) * 100.0,
                if t["shadow"] == true { " shadow" } else { "" },
                t["status"].as_str().unwrap_or(""),
            )
        })
        .collect()
}

/// `ancilo ab report`
pub fn ab_report(v: &Value) -> String {
    let mut out = ab_tests(&v["test"]);
    for arm in ["a", "b"] {
        let r = &v[arm];
        out.push_str(&format!(
            "  {} {:<26} done {:>4}/{:<4} success {:<24} no error {:<24} p50 {}\n",
            r["arm"].as_str().unwrap_or(""),
            r["model"].as_str().unwrap_or(""),
            r["completed"],
            r["assigned"],
            rate(&r["success"]),
            rate(&r["no_error"]),
            secs(&r["latency_p50_ms"]),
        ));
    }
    out.push_str(&format!("{}\n", v["summary"].as_str().unwrap_or("")));
    if let Some(reason) = v["test"]["end_reason"].as_str() {
        out.push_str(&format!("ended: {reason}\n"));
    }
    out
}

pub fn leaderboard(v: &Value) -> String {
    let entries = v["entries"].as_array().cloned().unwrap_or_default();
    let choices = v["choices"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() && choices.is_empty() {
        return "no results yet – run `ancilo compare` or `ancilo suite run`\n".into();
    }
    let mut out = String::new();
    let mut kind = "";
    for e in &entries {
        let k = e["kind"].as_str().unwrap_or("");
        if k != kind {
            out.push_str(&format!("{k}\n"));
            kind = k;
        }
        out.push_str(&format!(
            "  {:<28} {:<26} p50 {}\n",
            e["model"].as_str().unwrap_or(""),
            rate(&e["success"]),
            secs(&e["duration_p50_ms"])
        ));
    }
    if !choices.is_empty() {
        out.push_str("your choices in coding sessions (subjective)\n");
        for c in &choices {
            out.push_str(&format!(
                "  {:<28} taken {} · passed over {}\n",
                c["model"].as_str().unwrap_or(""),
                c["chosen"],
                c["passed_over"]
            ));
        }
    }
    out
}

pub fn recommendations(v: &Value) -> String {
    let list = match v {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    if list.is_empty() {
        return "no recommendations – they need enough comparable results\n".into();
    }
    list.iter()
        .map(|r| {
            let action = match r["action"]["type"].as_str() {
                Some("set_route") => format!(
                    "use {} for {} tasks",
                    r["action"]["model"].as_str().unwrap_or(""),
                    r["action"]["kind"].as_str().unwrap_or("")
                ),
                _ => format!(
                    "use {} for role {}",
                    r["action"]["model"].as_str().unwrap_or(""),
                    r["action"]["role"].as_str().unwrap_or("")
                ),
            };
            format!(
                "{} [{}] {action}\n  {}\n",
                r["id"].as_str().unwrap_or(""),
                r["status"].as_str().unwrap_or(""),
                r["rationale"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// `ancilo search`
pub fn search(v: &Value) -> String {
    let mut out = String::new();
    for h in v["hits"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "{}:{}-{}{}\n",
            h["path"].as_str().unwrap_or(""),
            h["start_line"],
            h["end_line"],
            h["symbol"]
                .as_str()
                .map(|s| format!("  {s}"))
                .unwrap_or_default()
        ));
        for line in h["snippet"].as_str().unwrap_or("").lines().take(6) {
            out.push_str(&format!("    {line}\n"));
        }
    }
    if out.is_empty() {
        out.push_str("no matches\n");
    }
    if let Some(n) = v["note"].as_str() {
        out.push_str(&format!("note: {n}\n"));
    }
    out
}

/// `ancilo index`, `ancilo knowledge`
pub fn index_status(v: &Value) -> String {
    if v["removed"] == true {
        return "index removed\n".into();
    }
    let mut out = format!(
        "{}\n  {} files · {} chunks · {} with embeddings · search: {}\n",
        v["name"].as_str().unwrap_or(""),
        v["files"],
        v["chunks"],
        v["embedded"],
        v["mode"].as_str().unwrap_or("")
    );
    if v["mode"] == "text" {
        out.push_str("  add an embedding model for semantic search, e.g. `ancilo add hf.co/second-state/All-MiniLM-L6-v2-Embedding-GGUF`\n");
    }
    if let Some(r) = v["last_refresh"].as_object() {
        out.push_str(&format!(
            "  last update: +{} ~{} −{} files, {} embedded, {} ms\n",
            r["added"], r["updated"], r["removed"], r["embedded"], r["duration_ms"]
        ));
    }
    out
}

/// `ancilo ask`
pub fn ask(v: &Value) -> String {
    let mut out = String::new();
    for o in v["operations"].as_array().into_iter().flatten() {
        let mark = match o["outcome"].as_str() {
            Some("executed") => "·",
            Some("proposed") => "?",
            _ => "✗",
        };
        out.push_str(&format!(
            "  {mark} {} {}\n",
            o["operation"].as_str().unwrap_or(""),
            short_args(&o["input"])
        ));
    }
    out.push_str(&format!("{}\n", v["answer"].as_str().unwrap_or("").trim()));
    out
}

/// `ancilo setup`
pub fn setup(v: &Value) -> String {
    let mut out = String::new();
    for (label, key) in [("chat model", "chat"), ("embedding model", "embed")] {
        let p = &v[key];
        if p.is_null() {
            continue;
        }
        out.push_str(&format!(
            "{label:<16} {} · {} · {}\n",
            p["repo"]["id"].as_str().unwrap_or(""),
            p["plan"]["quant"].as_str().unwrap_or("-"),
            if p["download_bytes"].as_u64().unwrap_or(0) == 0 {
                "already on disk".to_string()
            } else {
                format!("download {}", gb(num(&p["download_bytes"])))
            }
        ));
    }
    for n in v["notes"].as_array().into_iter().flatten() {
        out.push_str(&format!("  {}\n", n.as_str().unwrap_or("")));
    }
    if v["chat"].is_null() && v["embed"].is_null() {
        out.push_str("nothing to install\n");
    }
    out
}

/// `ancilo diagnose`
pub fn diagnose(v: &Value) -> String {
    let hw = &v["hardware"];
    let mut out = format!(
        "Ancilo {} · {} for models, {} in use\n",
        v["version"].as_str().unwrap_or(""),
        mem(num(&hw["model_budget_bytes"])),
        mem(num(&hw["used_bytes"]))
    );
    for m in v["models"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {:<28} {:<10} {}\n",
            m["id"].as_str().unwrap_or(""),
            m["status"].as_str().unwrap_or(""),
            m["roles"]
                .as_array()
                .map(|r| r
                    .iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ));
    }
    let calls = &v["calls_last_hour"];
    out.push_str(&format!(
        "last hour: {} requests, {} errors\n",
        calls["requests"], calls["errors"]
    ));
    for p in v["recent_problems"]
        .as_array()
        .into_iter()
        .flatten()
        .take(5)
    {
        out.push_str(&format!(
            "  ! {} {} {}\n",
            p["ts"].as_str().unwrap_or("").get(..19).unwrap_or(""),
            p["kind"].as_str().unwrap_or(""),
            p["subject"].as_str().unwrap_or("")
        ));
    }
    for h in v["hints"].as_array().into_iter().flatten() {
        out.push_str(&format!("hint: {}\n", h.as_str().unwrap_or("")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(status: &str) -> Value {
        json!({
            "id": "qwen3.6-35b-a3b-q8_0", "name": "Qwen3.6-35B-A3B", "quant": "Q8_0",
            "size_bytes": 36_903_140_320u64, "source": "Hugging Face · unsloth/Qwen3.6-35B-A3B-GGUF (existing file from LM Studio)",
            "path": "/Users/x/.lmstudio/models/unsloth/Qwen3.6-35B-A3B-GGUF/Qwen3.6-35B-A3B-Q8_0.gguf",
            "status": status, "roles": ["default"], "ctx_tokens": 32768, "expected_ram_bytes": 42_000_000_000u64,
            "fit": "fits", "download": {"bytes": 1, "total": 2, "percent": 50.0},
            "instance": {"port": 50000, "load_ms": 8400, "restarts": 0, "tokens_per_sec": 71.3},
            "embedding": false
        })
    }

    // covers: M1-AC-10
    #[test]
    fn list_output() {
        insta::assert_snapshot!(model_table(&json!([model("running"), {
            "id": "qwen3-embedding-0.6b-q8_0", "status": "downloading", "quant": "Q8_0", "size_bytes": 639_000_000,
            "ctx_tokens": 8192, "roles": ["embed"], "download": {"percent": 12.0}
        }])));
        insta::assert_snapshot!(model_table(&json!([])));
    }

    // covers: M1-AC-10
    #[test]
    fn detail_plan_progress_and_hardware_output() {
        insta::assert_snapshot!(model_detail(&model("running")));
        insta::assert_snapshot!(plan_summary(&json!({
            "plan": {"files": [{"path": "Qwen3.6-35B-A3B-Q8_0.gguf"}], "quant": "Q8_0", "size_bytes": 36_903_140_320u64,
                     "ctx_tokens": 32768, "fit": "fits", "reason": "Q8_0 needs about 39.1 GB of 92.9 GB available – fits comfortably"},
            "existing": null, "existing_in": null, "download_bytes": 36_903_140_320u64
        })));
        insta::assert_snapshot!(progress_line(
            &json!({"bytes": 12_000_000_000u64, "total": 36_903_140_320u64, "percent": 32.5, "bytes_per_sec": 3_100_000})
        ));
        insta::assert_snapshot!(hardware(&json!({
            "hardware": {"chip": "Apple M5 Max", "total_ram_bytes": 137_438_953_472u64, "gpu": "metal",
                         "performance_cores": 12, "total_cores": 18, "free_disk_bytes": 800_000_000_000u64},
            "model_budget_bytes": 103_079_215_104u64, "used_bytes": 42_000_000_000u64, "free_for_models_bytes": 61_079_215_104u64
        })));
    }

    // covers: M1-AC-10
    #[test]
    fn error_output() {
        insta::assert_snapshot!(error(
            "insufficient_resources",
            "This model does not fit on this machine: even the smallest variant needs about 26.4 GB, but only 25.8 GB are available. Try a smaller context or a smaller model."
        ));
        insta::assert_snapshot!(error(
            "insufficient_resources",
            "not enough disk space: the download needs 36.9 GB, 20.0 GB are free"
        ));
        insta::assert_snapshot!(error(
            "not_found",
            "repository 'org/nope' was not found on Hugging Face"
        ));
        insta::assert_snapshot!(error(
            "insufficient_resources",
            "'b' needs about 40.0 GB, but only 20.0 GB of 96.0 GB are free (a loaded). Stop another model first."
        ));
    }
}
