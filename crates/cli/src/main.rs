//! `ancilo` – the command line. One binary: CLI, daemon (`ancilo daemon run`)
//! and, later, the MCP bridge.

mod client;
mod fmt;

use std::process::ExitCode;

use ancilo_core::{Config, Error, Paths, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use serde_json::{Value, json};

use client::{Client, StatusLine};

#[derive(Parser)]
#[command(name = "ancilo", version, about = "Ancilo – local LLMs made simple", long_about = None)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum LlamaAction {
    /// Download and install the pinned llama.cpp build (packages already contain it).
    Install,
}

#[derive(Subcommand)]
enum Command {
    /// Add a model and start it: `ancilo add hf.co/unsloth/Qwen3.6-35B-A3B-GGUF`
    Add {
        /// Hugging Face repo (optionally `:Q4_K_M`), GGUF file, or server URL.
        address: String,
        /// small | medium | large (klein | mittel | groß)
        #[arg(long, short)]
        context: Option<String>,
        /// Use this quantization, e.g. Q4_K_M.
        #[arg(long)]
        quant: Option<String>,
        /// Only add, do not start.
        #[arg(long)]
        no_start: bool,
        /// Return immediately instead of following progress.
        #[arg(long)]
        no_wait: bool,
        /// Server addresses: which of the server's models.
        #[arg(long, short)]
        model: Option<String>,
    },
    /// Show how a model would run here, without adding it.
    Plan {
        address: String,
        #[arg(long, short)]
        context: Option<String>,
        #[arg(long)]
        quant: Option<String>,
    },
    /// List models.
    #[command(alias = "ls")]
    List,
    /// Show one model.
    Status { model: String },
    /// Load a model into memory.
    Start {
        model: String,
        #[arg(long, short)]
        context: Option<String>,
        #[arg(long)]
        no_wait: bool,
    },
    /// Unload a model.
    Stop { model: String },
    /// Remove a model (deletes files Ancilo downloaded).
    #[command(alias = "rm")]
    Remove {
        model: String,
        #[arg(long)]
        keep_files: bool,
    },
    /// Use a model for a role (default, delegation, coding, assistant, embed, …).
    #[command(alias = "assign")]
    Role { role: String, model: String },
    /// Rules per task kind: `ancilo route` lists, `ancilo route tests qwen-coder` sets,
    /// `ancilo route tests --remove` removes.
    Route {
        /// tests | refactor | fix | docs | summary | other
        kind: Option<String>,
        /// Model (an arrow `→` / `->` before it is allowed).
        model: Vec<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Run the same task on several models and compare:
    /// `ancilo compare "write tests for src/parser.rs" -m a,b --check "cargo test"`
    Compare {
        task: String,
        /// Models, comma-separated.
        #[arg(long, short, value_delimiter = ',', required = true)]
        models: Vec<String>,
        /// Command that decides success (runs in the sandbox).
        #[arg(long)]
        check: Option<String>,
        /// Allow network access for the check.
        #[arg(long)]
        check_network: bool,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Hide which model is which until you rate the result.
        #[arg(long)]
        blind: bool,
        #[arg(long)]
        kind: Option<String>,
        /// Judge model (rates results 1–10; marked as model-based).
        #[arg(long)]
        judge: Option<String>,
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// read | edit | shell
        #[arg(long, default_value = "edit")]
        allow: String,
        /// Return the comparison id at once.
        #[arg(long)]
        no_wait: bool,
    },
    /// List comparisons, or show one: `ancilo comparisons c-…`
    Comparisons { id: Option<String> },
    /// Rate a comparison (reveals a blind one): `ancilo rate c-… A`
    Rate {
        id: String,
        /// Best label or `tie`.
        best: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Personal benchmarks: save, list and run task suites.
    Suite {
        #[command(subcommand)]
        action: SuiteAction,
    },
    /// A/B tests in live use.
    Ab {
        #[command(subcommand)]
        action: AbAction,
    },
    /// Search the project (default) or Ancilo's knowledge base:
    /// `ancilo search "where is the token checked"`
    Search {
        query: String,
        /// Search Ancilo's knowledge base instead of the project.
        #[arg(long, short)]
        knowledge: bool,
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// Index the project for search (or show / remove its index).
    Index {
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        #[arg(long)]
        status: bool,
        #[arg(long)]
        remove: bool,
    },
    /// Rebuild Ancilo's knowledge base (docs, model notes and cards, results).
    Knowledge {
        /// Only show its state.
        #[arg(long)]
        status: bool,
    },
    /// Ask Ancilo's assistant: `ancilo ask "set up a coding model and connect it to Claude Code"`
    Ask {
        prompt: String,
        /// Confirm proposed actions without asking.
        #[arg(long, short)]
        yes: bool,
        #[arg(long, short)]
        model: Option<String>,
    },
    /// First start: install a chat model that fits this machine and an embedding model.
    Setup {
        /// Only show what would be installed.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// Open the app in the browser (the same UI as the desktop app).
    Ui {
        /// Only print the address.
        #[arg(long)]
        print: bool,
    },
    /// What is going on? Hardware, models, recent errors, hints.
    Diagnose,
    /// The last lines of a model's log.
    Logs {
        model: String,
        #[arg(long, short, default_value_t = 50)]
        lines: usize,
    },
    /// Add a cloud model (OpenAI-compatible provider); the key is read from
    /// ANCILO_API_KEY or standard input and stored in the system keychain.
    Cloud {
        /// API base, e.g. https://api.deepinfra.com/v1/openai
        base_url: String,
        /// The provider's model name.
        model: String,
        #[arg(long, short)]
        yes: bool,
    },
    /// Models ranked per task kind on this machine.
    Leaderboard {
        #[arg(long)]
        kind: Option<String>,
        /// Print the Markdown export.
        #[arg(long)]
        markdown: bool,
    },
    /// Suggested model assignments: list, `apply <id>` or `dismiss <id>`.
    #[command(alias = "recommend")]
    Recommendations {
        /// apply | dismiss
        action: Option<String>,
        id: Option<String>,
        /// Include applied and dismissed ones.
        #[arg(long)]
        all: bool,
    },
    /// Show hardware and memory available for models.
    Hardware,
    /// llama.cpp, which runs the models: `ancilo llama install`
    Llama {
        #[command(subcommand)]
        action: LlamaAction,
    },
    /// Delegate a task to the local model: `ancilo run "write tests for src/parser.rs"`
    Run {
        task: String,
        /// Project directory (default: current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// read | edit | shell
        #[arg(long, default_value = "edit")]
        allow: String,
        /// Work on a separate git branch and return at once.
        #[arg(long)]
        background: bool,
        #[arg(long, short)]
        model: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// List delegated tasks.
    Tasks,
    /// Show a delegated task.
    Task {
        id: String,
        /// Include the diff.
        #[arg(long)]
        diff: bool,
        /// Wait for the task to finish.
        #[arg(long)]
        wait: bool,
    },
    /// Cancel a delegated task.
    Cancel { id: String },
    /// Let Claude Code or Codex delegate to Ancilo: `ancilo connect claude`
    Connect { client: String },
    /// Remove Ancilo from Claude Code or Codex.
    Disconnect { client: String },
    /// MCP server on stdin/stdout (started by Claude Code, Codex, …).
    Mcp,
    /// Print the path of the Ancilo plugin for Claude Code (try it without
    /// installing: `claude --plugin-dir "$(ancilo claude-plugin)"`).
    ClaudePlugin,
    /// Start Claude Code on the local model (extra arguments are passed on).
    #[command(trailing_var_arg = true)]
    Claude {
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start Codex on the local model (extra arguments are passed on).
    #[command(trailing_var_arg = true)]
    Codex {
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run an eval suite: `ancilo eval tool-calling --model default --reliability off`
    Eval {
        /// Built-in suite (tool-calling, no-tool, delegation, coding, …) or path to a YAML suite.
        suite: String,
        #[arg(long, short)]
        model: Option<String>,
        /// all | off | comma-separated stages
        #[arg(long, short, default_value = "all")]
        reliability: String,
        #[arg(long, default_value_t = 3)]
        repeat: u32,
    },
    /// Call any operation: `ancilo op list_models` or `ancilo op plan_model '{"address": "…"}'`
    Op {
        name: String,
        /// JSON input (default: {}).
        input: Option<String>,
        /// Confirm a consequential operation.
        #[arg(long)]
        confirm: bool,
    },
    /// List all operations.
    Ops,
    /// Follow events.
    Events {
        #[arg(long)]
        subject: Option<String>,
    },
    /// Manage the background daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum SuiteAction {
    /// Save a YAML suite under a name.
    Save {
        name: String,
        file: std::path::PathBuf,
    },
    /// List built-in and saved suites.
    List,
    /// Run a suite on several models.
    Run {
        suite: String,
        #[arg(long, short, value_delimiter = ',', required = true)]
        models: Vec<String>,
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        #[arg(long)]
        no_wait: bool,
    },
}

#[derive(Subcommand)]
enum AbAction {
    /// `ancilo ab start delegation --b devstral --share 20%`
    Start {
        role: String,
        #[arg(long)]
        b: String,
        /// Default: the model the role uses now.
        #[arg(long)]
        a: Option<String>,
        /// Share for B, e.g. `20%` or `0.2`.
        #[arg(long, default_value = "20%")]
        share: String,
        #[arg(long)]
        seed: Option<u64>,
        /// B also does every delegation in its own worktree (no effect on results).
        #[arg(long)]
        shadow: bool,
    },
    /// Stop a test (id or role).
    Stop { test: String },
    /// List tests.
    Status,
    /// Results with confidence intervals (id or role).
    Report { test: String },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Run in the foreground.
    Run,
    /// Start in the background (if not running).
    Start,
    /// Stop the daemon and all models.
    Stop,
    /// Show whether the daemon runs.
    Status,
}

fn context_arg(c: &Option<String>) -> Result<Option<Value>> {
    match c {
        None => Ok(None),
        Some(s) => ancilo_models::planner::ContextSize::parse(s)
            .map(|c| Some(serde_json::to_value(c).unwrap_or_default()))
            .ok_or_else(|| {
                Error::invalid(format!("context must be small, medium or large, not '{s}'"))
            }),
    }
}

/// Asks a yes/no question on the terminal (no in non-interactive use).
fn confirm_prompt(question: &str) -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).ok();
    matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "j" | "ja"
    )
}

/// `20%`, `20` or `0.2` → 0.2
fn parse_share(s: &str) -> Result<f64> {
    let t = s.trim();
    let (num, pct) = match t.strip_suffix('%') {
        Some(n) => (n.trim(), true),
        None => (t, false),
    };
    let v: f64 = num
        .parse()
        .map_err(|_| Error::invalid(format!("share must be like 20% or 0.2, not '{s}'")))?;
    Ok(if pct || v > 1.0 { v / 100.0 } else { v })
}

/// Waits for a comparison and shows its progress.
async fn wait_comparison(client: &Client, id: &str) -> Result<Value> {
    let mut line = StatusLine::new();
    loop {
        let v = client
            .call("comparison_report", json!({"id": id, "wait_s": 2}), false)
            .await?;
        let status = v["status"].as_str().unwrap_or_default();
        if !matches!(status, "queued" | "running") {
            line.finish();
            return Ok(v);
        }
        line.update(&format!(
            "{id} {status} · {}",
            v["progress"].as_str().unwrap_or("")
        ));
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Follows events for a model until it runs (or fails).
async fn follow(
    client: &Client,
    id: &str,
    events: impl futures::Stream<Item = ancilo_core::Event>,
    want_running: bool,
) -> Result<()> {
    let mut line = StatusLine::new();
    futures::pin_mut!(events);
    // The state might already be final.
    let current = client
        .call("model_status", json!({"model": id}), false)
        .await?;
    if current["status"] == "running" || (!want_running && current["status"] == "ready") {
        return Ok(());
    }
    while let Some(e) = events.next().await {
        if e.subject.as_deref() != Some(id) && e.subject.as_deref() != Some("llama.cpp") {
            continue;
        }
        match e.kind.as_str() {
            "download.progress" if e.subject.as_deref() == Some(id) => {
                line.update(&fmt::progress_line(&e.data))
            }
            "download.progress" => line.update(&format!(
                "installing llama.cpp  {}",
                fmt::progress_line(&e.data)
            )),
            "download.retrying" => line.update(&format!(
                "connection problem, retrying: {}",
                e.data["reason"].as_str().unwrap_or("")
            )),
            "model.downloaded" => {
                line.update("download complete, checksum verified");
                line.finish();
                if !want_running {
                    return Ok(());
                }
            }
            "instance.starting" => line.update("loading model …"),
            "instance.ready" => line.update(&format!(
                "loaded in {:.1} s, measuring speed …",
                e.data["load_ms"].as_f64().unwrap_or(0.0) / 1000.0
            )),
            "model.running" => {
                line.finish();
                match e.data["tokens_per_sec"].as_f64() {
                    Some(t) => eprintln!("✓ {id} is running · {t:.0} tokens/s"),
                    None => eprintln!("✓ {id} is running"),
                }
                return Ok(());
            }
            "model.failed" | "instance.failed" | "download.failed" => {
                line.finish();
                return Err(Error::unavailable(
                    e.data["reason"].as_str().unwrap_or("failed").to_string(),
                ));
            }
            _ => {}
        }
    }
    line.finish();
    Err(Error::unavailable("lost connection to the daemon"))
}

async fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve();
    if let Command::Daemon {
        action: DaemonAction::Run,
    } = &cli.command
    {
        let config = Config::load(&paths)?;
        return ancilo_daemon::run(paths, config).await;
    }
    if let Command::Daemon { action } = &cli.command {
        match action {
            DaemonAction::Status => match Client::connect(&paths, false).await {
                Ok(c) => {
                    let v = c.call("daemon_info", json!({}), false).await?;
                    if cli.json {
                        print_json(&v)
                    } else {
                        println!(
                            "running · {} · version {} · pid {}",
                            v["url"].as_str().unwrap_or(""),
                            v["version"].as_str().unwrap_or(""),
                            v["pid"]
                        )
                    }
                }
                Err(_) => println!("not running"),
            },
            DaemonAction::Start => {
                let c = Client::connect(&paths, true).await?;
                println!("running · {}", c.url);
            }
            DaemonAction::Stop => match Client::connect(&paths, false).await {
                Ok(c) => {
                    c.stop_and_wait(&paths).await?;
                    println!("stopped");
                }
                Err(_) => println!("not running"),
            },
            DaemonAction::Run => unreachable!(),
        }
        return Ok(());
    }

    if let Command::Mcp = &cli.command {
        return mcp_bridge(&paths).await;
    }
    let client = Client::connect(&paths, true).await?;
    match cli.command {
        Command::Add {
            address,
            context,
            quant,
            no_start,
            no_wait,
            model,
        } => {
            // A server (Ollama, LM Studio, …): nothing to plan or download.
            if address.starts_with("http://") || address.starts_with("https://") {
                let v = client
                    .call(
                        "add_model",
                        json!({"address": address, "model": model}),
                        true,
                    )
                    .await?;
                if cli.json {
                    print_json(&v)
                } else {
                    println!("{}", fmt::model_detail(&v).trim_end())
                }
                return Ok(());
            }
            let ctx = context_arg(&context)?;
            let mut plan_input = json!({"address": address, "quant": quant});
            if let Some(c) = &ctx {
                plan_input["context"] = c.clone();
            }
            let plan = client.call("plan_model", plan_input.clone(), false).await?;
            if !cli.json {
                eprint!("{}", fmt::plan_summary(&plan));
            }
            let mut input = plan_input;
            input["start"] = json!(!no_start);
            // Subscribe before adding so no progress event is missed.
            let events = client.events(None).await?;
            let view = client.call("add_model", input, true).await?;
            let id = view["id"].as_str().unwrap_or_default().to_string();
            if !no_wait {
                follow(&client, &id, events, !no_start).await?;
            }
            let view = client
                .call("model_status", json!({"model": id}), false)
                .await?;
            if cli.json {
                print_json(&view)
            } else if no_wait {
                println!("{}", fmt::model_detail(&view).trim_end())
            }
        }
        Command::Plan {
            address,
            context,
            quant,
        } => {
            let mut input = json!({"address": address, "quant": quant});
            if let Some(c) = context_arg(&context)? {
                input["context"] = c;
            }
            let v = client.call("plan_model", input, false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::plan_summary(&v))
            }
        }
        Command::List => {
            let v = client.call("list_models", json!({}), false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::model_table(&v))
            }
        }
        Command::Status { model } => {
            let v = client
                .call("model_status", json!({"model": model}), false)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::model_detail(&v))
            }
        }
        Command::Start {
            model,
            context,
            no_wait,
        } => {
            let mut input = json!({"model": model});
            if let Some(c) = context_arg(&context)? {
                input["context"] = c;
            }
            let events = client.events(None).await?;
            let v = client.call("start_model", input, true).await?;
            let id = v["id"].as_str().unwrap_or_default().to_string();
            if !no_wait {
                follow(&client, &id, events, true).await?;
            }
            if cli.json {
                print_json(
                    &client
                        .call("model_status", json!({"model": id}), false)
                        .await?,
                )
            }
        }
        Command::Stop { model } => {
            let v = client
                .call("stop_model", json!({"model": model}), true)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                println!("stopped {}", v["id"].as_str().unwrap_or(""))
            }
        }
        Command::Remove { model, keep_files } => {
            let v = client
                .call(
                    "remove_model",
                    json!({"model": model, "keep_files": keep_files}),
                    true,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                let n = v["deleted_files"].as_array().map_or(0, Vec::len);
                println!(
                    "removed {model}{}",
                    if n > 0 {
                        format!(" ({n} file(s) deleted)")
                    } else {
                        String::new()
                    }
                );
            }
        }
        Command::Role { role, model } => {
            let v = client
                .call("assign_role", json!({"role": role, "model": model}), true)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                println!(
                    "{} is now used for '{role}'",
                    v["id"].as_str().unwrap_or("")
                )
            }
        }
        Command::Route {
            kind,
            model,
            remove,
        } => {
            let model: Vec<String> = model
                .into_iter()
                .filter(|m| m != "→" && m != "->")
                .collect();
            let v = match (kind, model.first(), remove) {
                (Some(k), _, true) => {
                    client
                        .call("remove_route", json!({"kind": k}), true)
                        .await?;
                    client.call("list_routes", json!({}), false).await?
                }
                (Some(k), Some(m), false) => {
                    client
                        .call("set_route", json!({"kind": k, "model": m}), true)
                        .await?;
                    client.call("list_routes", json!({}), false).await?
                }
                (Some(k), None, false) => {
                    client
                        .call("explain_route", json!({"kind": k}), false)
                        .await?
                }
                (None, _, _) => client.call("list_routes", json!({}), false).await?,
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::routes(&v))
            }
        }
        Command::Compare {
            task,
            models,
            check,
            check_network,
            repeat,
            blind,
            kind,
            judge,
            cwd,
            allow,
            no_wait,
        } => {
            let cwd = match cwd {
                Some(c) => std::fs::canonicalize(c)?,
                None => std::env::current_dir()?,
            };
            let v = client
                .call(
                    "compare_models",
                    json!({"task": task, "cwd": cwd, "models": models, "check": check,
                        "check_network": check_network, "repeat": repeat, "blind": blind,
                        "kind": kind, "judge": judge, "allow": allow}),
                    true,
                )
                .await?;
            let id = v["id"].as_str().unwrap_or_default().to_string();
            let v = if no_wait {
                v
            } else {
                wait_comparison(&client, &id).await?
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::comparison(&v))
            }
        }
        Command::Comparisons { id: Some(id) } => {
            let v = client
                .call("comparison_report", json!({"id": id}), false)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::comparison(&v))
            }
        }
        Command::Comparisons { id: None } => {
            let v = client.call("list_comparisons", json!({}), false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::comparison_list(&v))
            }
        }
        Command::Rate { id, best, note } => {
            let v = client
                .call(
                    "rate_comparison",
                    json!({"id": id, "best": best, "note": note}),
                    true,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::comparison(&v))
            }
        }
        Command::Suite { action } => {
            let v = match action {
                SuiteAction::Save { name, file } => {
                    let content = std::fs::read_to_string(&file)
                        .map_err(|e| Error::invalid(format!("{}: {e}", file.display())))?;
                    client
                        .call(
                            "save_suite",
                            json!({"name": name, "content": content}),
                            true,
                        )
                        .await?
                }
                SuiteAction::List => client.call("list_suites", json!({}), false).await?,
                SuiteAction::Run {
                    suite,
                    models,
                    repeat,
                    no_wait,
                } => {
                    let v = client
                        .call(
                            "run_suite",
                            json!({"suite": suite, "models": models, "repeat": repeat}),
                            true,
                        )
                        .await?;
                    let id = v["id"].as_str().unwrap_or_default().to_string();
                    let v = if no_wait {
                        v
                    } else {
                        wait_comparison(&client, &id).await?
                    };
                    if !cli.json {
                        print!("{}", fmt::comparison(&v));
                        return Ok(());
                    }
                    v
                }
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::suites(&v))
            }
        }
        Command::Ab { action } => {
            let v = match action {
                AbAction::Start {
                    role,
                    b,
                    a,
                    share,
                    seed,
                    shadow,
                } => {
                    let share = parse_share(&share)?;
                    client
                        .call(
                            "ab_start",
                            json!({"role": role, "b": b, "a": a, "share": share, "seed": seed, "shadow": shadow}),
                            true,
                        )
                        .await?
                }
                AbAction::Stop { test } => {
                    client.call("ab_stop", json!({"test": test}), true).await?
                }
                AbAction::Status => client.call("ab_status", json!({}), false).await?,
                AbAction::Report { test } => {
                    let v = client
                        .call("ab_report", json!({"test": test}), false)
                        .await?;
                    if !cli.json {
                        print!("{}", fmt::ab_report(&v));
                        return Ok(());
                    }
                    v
                }
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::ab_tests(&v))
            }
        }
        Command::Search {
            query,
            knowledge,
            cwd,
            limit,
        } => {
            let mut input = json!({"query": query, "limit": limit});
            if knowledge {
                input["scope"] = json!("knowledge");
            } else {
                let cwd = match cwd {
                    Some(c) => std::fs::canonicalize(c)?,
                    None => std::env::current_dir()?,
                };
                input["cwd"] = json!(cwd);
            }
            let v = client.call("search", input, false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::search(&v))
            }
        }
        Command::Index {
            cwd,
            status,
            remove,
        } => {
            let cwd = match cwd {
                Some(c) => std::fs::canonicalize(c)?,
                None => std::env::current_dir()?,
            };
            let op = if remove {
                "remove_index"
            } else if status {
                "index_status"
            } else {
                "index_project"
            };
            let v = client
                .call(op, json!({"cwd": cwd}), op != "index_status")
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::index_status(&v))
            }
        }
        Command::Knowledge { status } => {
            let v = if status {
                client.call("knowledge_status", json!({}), false).await?
            } else {
                client
                    .call("refresh_model_knowledge", json!({}), true)
                    .await?
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::index_status(v.get("index").unwrap_or(&v)))
            }
        }
        Command::Ask { prompt, yes, model } => {
            let v = client
                .call("ask", json!({"prompt": prompt, "model": model}), false)
                .await?;
            if cli.json && !yes {
                print_json(&v);
                return Ok(());
            }
            if !cli.json {
                print!("{}", fmt::ask(&v));
            }
            for p in v["pending"].as_array().into_iter().flatten() {
                let id = p["id"].as_str().unwrap_or_default();
                let go = yes
                    || confirm_prompt(&format!(
                        "Run {} {}?",
                        p["operation"].as_str().unwrap_or(""),
                        fmt::short_args(&p["input"])
                    ));
                if go {
                    let r = client
                        .call("confirm_action", json!({"id": id}), true)
                        .await?;
                    if cli.json {
                        print_json(&r)
                    } else {
                        println!("✓ {}", p["operation"].as_str().unwrap_or(""));
                    }
                } else {
                    client
                        .call("reject_action", json!({"id": id}), false)
                        .await?;
                    println!("skipped");
                }
            }
        }
        Command::Setup { dry_run, yes } => {
            let plan = client.call("setup", json!({"dry_run": true}), true).await?;
            print!("{}", fmt::setup(&plan));
            if dry_run || (plan["chat"].is_null() && plan["embed"].is_null()) {
                return Ok(());
            }
            if !yes && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                return Err(Error::invalid(
                    "nothing installed – confirm with `ancilo setup --yes` when not running in a terminal",
                ));
            }
            if yes || confirm_prompt("Install?") {
                let events = client.events(None).await?;
                let v = client.call("setup", json!({}), true).await?;
                for m in v["added"].as_array().into_iter().flatten() {
                    let id = m["id"].as_str().unwrap_or_default().to_string();
                    let events = client.events(None).await?;
                    follow(
                        &client,
                        &id,
                        events,
                        !m["embedding"].as_bool().unwrap_or(false),
                    )
                    .await
                    .ok();
                }
                drop(events);
                println!("ready – try `ancilo ask \"what can you do?\"`");
            }
        }
        Command::Ui { print } => {
            let info = ancilo_daemon::DaemonInfo::read(&paths)
                .ok_or_else(|| Error::unavailable("the daemon is not running"))?;
            let token = std::fs::read_to_string(paths.token_file())?;
            // The token travels in the fragment: browsers never send it to a server.
            let url = format!("{}/app/#token={}", info.url, token.trim());
            if print {
                println!("{url}");
            } else {
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                std::process::Command::new(opener)
                    .arg(&url)
                    .status()
                    .map_err(|e| Error::unavailable(format!("cannot open a browser: {e}")))?;
            }
        }
        Command::Diagnose => {
            let v = client.call("diagnose", json!({}), false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::diagnose(&v))
            }
        }
        Command::Logs { model, lines } => {
            let v = client
                .call("model_logs", json!({"model": model, "lines": lines}), false)
                .await?;
            for l in v["lines"].as_array().into_iter().flatten() {
                println!("{}", l.as_str().unwrap_or(""));
            }
        }
        Command::Cloud {
            base_url,
            model,
            yes,
        } => {
            let key = match std::env::var("ANCILO_API_KEY") {
                Ok(k) => k,
                Err(_) => {
                    eprint!("API key (input is not shown in logs; press Enter when done): ");
                    let mut k = String::new();
                    std::io::stdin().read_line(&mut k)?;
                    k.trim().to_string()
                }
            };
            println!("{}", ancilo_models::ops::CLOUD_PRIVACY);
            if !(yes || confirm_prompt("Add this cloud model?")) {
                return Ok(());
            }
            let v = client
                .call(
                    "set_cloud_provider",
                    json!({"base_url": base_url, "model": model, "api_key": key}),
                    true,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                println!(
                    "added {} (cloud) – give it a role explicitly, e.g. `ancilo assign assistant {}`",
                    v["model"]["id"].as_str().unwrap_or(""),
                    v["model"]["id"].as_str().unwrap_or("")
                );
            }
        }
        Command::Leaderboard { kind, markdown } => {
            let v = client
                .call("leaderboard", json!({"kind": kind}), false)
                .await?;
            if cli.json {
                print_json(&v)
            } else if markdown {
                print!("{}", v["markdown"].as_str().unwrap_or_default())
            } else {
                print!("{}", fmt::leaderboard(&v))
            }
        }
        Command::Recommendations { action, id, all } => {
            let v = match (action.as_deref(), id) {
                (Some("apply"), Some(id)) => {
                    client
                        .call("apply_recommendation", json!({"id": id}), true)
                        .await?
                }
                (Some("dismiss"), Some(id)) => {
                    client
                        .call("dismiss_recommendation", json!({"id": id}), true)
                        .await?
                }
                (Some(a), _) => {
                    return Err(Error::invalid(format!(
                        "use `ancilo recommendations apply <id>` or `dismiss <id>` (got '{a}')"
                    )));
                }
                (None, _) => {
                    client
                        .call("recommendations", json!({"all": all}), false)
                        .await?
                }
            };
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::recommendations(&v))
            }
        }
        Command::Llama {
            action: LlamaAction::Install,
        } => {
            let v = client.call("install_llama", json!({}), true).await?;
            if cli.json {
                print_json(&v)
            } else {
                println!(
                    "llama.cpp {} installed: {}",
                    v["tag"].as_str().unwrap_or(""),
                    v["path"].as_str().unwrap_or("")
                )
            }
        }
        Command::Hardware => {
            let v = client.call("hardware_info", json!({}), false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::hardware(&v))
            }
        }
        Command::Op {
            name,
            input,
            confirm,
        } => {
            let input: Value = match input {
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| Error::invalid(format!("input is not JSON: {e}")))?,
                None => json!({}),
            };
            print_json(&client.call(&name, input, confirm).await?);
        }
        Command::Eval {
            suite,
            model,
            repeat,
            ..
        } if suite.starts_with("delegation") || suite.ends_with("delegation.yaml") => {
            let v = client
                .call(
                    "run_delegation_eval",
                    json!({"suite": suite, "model": model, "repeat": repeat}),
                    true,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::delegation_report(&v))
            }
        }
        Command::Eval {
            suite,
            model,
            repeat,
            ..
        } if suite == "coding" || suite.ends_with("coding.yaml") => {
            let v = client
                .call(
                    "run_coding_eval",
                    json!({"suite": suite, "model": model, "repeat": repeat}),
                    true,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::delegation_report(&v))
            }
        }
        Command::Eval {
            suite,
            model,
            reliability,
            repeat,
        } => {
            let v = client
                .call("run_eval", json!({"suite": suite, "model": model, "reliability": reliability, "repeat": repeat}), true)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::eval_report(&v))
            }
        }
        Command::Claude { args } => {
            let cfg = client
                .call("model_api_config", json!({"client": "claude_code"}), false)
                .await?;
            let mut cmd = std::process::Command::new("claude");
            for (k, v) in cfg["env"].as_object().into_iter().flatten() {
                cmd.env(k, v.as_str().unwrap_or_default());
            }
            if let Some(dir) = cfg["env"]["CLAUDE_CONFIG_DIR"].as_str() {
                std::fs::create_dir_all(dir)?;
            }
            // Local models are unknown to Claude Code's model catalog.
            cmd.env("CLAUDE_CODE_MAX_CONTEXT_TOKENS", "131072");
            let status = cmd.args(&args).status().map_err(|e| {
                Error::unavailable(format!("cannot start Claude Code (`claude`): {e}"))
            })?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Command::Codex { args } => {
            let cfg = client
                .call("model_api_config", json!({"client": "codex"}), false)
                .await?;
            let base = cfg["base_url"].as_str().unwrap_or_default();
            let status = std::process::Command::new("codex")
                .env("ANCILO_TOKEN", cfg["env"]["ANCILO_TOKEN"].as_str().unwrap_or_default())
                .arg("-c")
                .arg("model_provider=ancilo")
                .arg("-c")
                .arg(format!(
                    "model_providers.ancilo={{name=\"Ancilo\", base_url=\"{base}/v1\", env_key=\"ANCILO_TOKEN\", wire_api=\"responses\"}}"
                ))
                .arg("-m")
                .arg("default")
                .args(&args)
                .status()
                .map_err(|e| Error::unavailable(format!("cannot start Codex (`codex`): {e}")))?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Command::Run {
            task,
            cwd,
            allow,
            background,
            model,
            kind,
        } => {
            let cwd = match cwd {
                Some(c) => std::fs::canonicalize(c)?,
                None => std::env::current_dir()?,
            };
            let mut input =
                json!({"task": task, "cwd": cwd, "allow": allow, "background": background});
            if let Some(m) = model {
                input["model"] = json!(m);
            }
            if let Some(k) = kind {
                input["kind"] = json!(k);
            }
            let events = client.events(None).await?;
            let call = client.call("delegate", input, true);
            futures::pin_mut!(events);
            futures::pin_mut!(call);
            // Show what the worker does while it works.
            let mut line = StatusLine::new();
            let v = loop {
                tokio::select! {
                    r = &mut call => break r?,
                    Some(e) = events.next() => {
                        match e.kind.as_str() {
                            "agent.tool_called" => line.update(&format!("{} {}", e.data["name"].as_str().unwrap_or(""), fmt::short_args(&e.data["arguments"]))),
                            "agent.file_changed" => line.update(&format!("{} {}", e.data["kind"].as_str().unwrap_or(""), e.data["path"].as_str().unwrap_or(""))),
                            _ => {}
                        }
                    }
                }
            };
            line.finish();
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::task(&v))
            }
        }
        Command::Tasks => {
            let v = client.call("list_tasks", json!({}), false).await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::task_table(&v))
            }
        }
        Command::Task { id, diff, wait } => {
            let v = client
                .call(
                    "task_result",
                    json!({"task_id": id, "detail": diff, "wait_s": if wait { 3600 } else { 0 }}),
                    false,
                )
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::task(&v))
            }
        }
        Command::Cancel { id } => {
            let v = client
                .call("cancel_task", json!({"task_id": id}), true)
                .await?;
            if cli.json {
                print_json(&v)
            } else {
                print!("{}", fmt::task(&v))
            }
        }
        Command::Connect { client: target } => connect(&client, &target, false, cli.json).await?,
        Command::Disconnect { client: target } => connect(&client, &target, true, cli.json).await?,
        Command::Mcp => unreachable!(),
        Command::ClaudePlugin => {
            let v = client.call("claude_plugin_dir", json!({}), true).await?;
            println!("{}", v["path"].as_str().unwrap_or_default());
        }
        Command::Ops => {
            let v = client.get("/api/v1/ops").await?;
            if cli.json {
                print_json(&v)
            } else {
                for op in v.as_array().into_iter().flatten() {
                    println!(
                        "{:<18} {}",
                        op["name"].as_str().unwrap_or(""),
                        op["summary"].as_str().unwrap_or("")
                    );
                }
            }
        }
        Command::Events { subject } => {
            let events = client.events(subject.as_deref()).await?;
            futures::pin_mut!(events);
            while let Some(e) = events.next().await {
                println!("{}", serde_json::to_string(&e).unwrap_or_default());
            }
        }
        Command::Daemon { .. } => unreachable!(),
    }
    Ok(())
}

async fn connect(client: &Client, target: &str, disconnect: bool, json_out: bool) -> Result<()> {
    let op = match (target, disconnect) {
        ("claude" | "claude-code" | "claude_code", false) => "connect_claude_code",
        ("claude" | "claude-code" | "claude_code", true) => "disconnect_claude_code",
        ("codex", false) => "connect_codex",
        ("codex", true) => "disconnect_codex",
        (other, _) => {
            return Err(Error::invalid(format!(
                "unknown client '{other}' – use claude or codex"
            )));
        }
    };
    let v = client.call(op, json!({}), true).await?;
    if json_out {
        print_json(&v)
    } else {
        print!("{}", fmt::connect(&v))
    }
    Ok(())
}

/// stdio ⇄ the daemon's MCP endpoint. Only protocol messages go to stdout.
async fn mcp_bridge(paths: &Paths) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let client = Client::connect(paths, true).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(Error::internal)? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match client.mcp(&line).await {
            Ok(r) => r,
            Err(e) => {
                // Answer requests with a JSON-RPC error instead of hanging the client.
                let id = serde_json::from_str::<Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").cloned());
                id.map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": e.message()}}).to_string())
            }
        };
        if let Some(r) = reply {
            out.write_all(r.as_bytes()).await.map_err(Error::internal)?;
            out.write_all(b"\n").await.map_err(Error::internal)?;
            out.flush().await.map_err(Error::internal)?;
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let daemon = matches!(
        cli.command,
        Command::Daemon {
            action: DaemonAction::Run
        }
    );
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ANCILO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(if daemon { "info" } else { "warn" })
            }),
        )
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", fmt::error(e.code(), &e.message()));
            ExitCode::from(if e.code() == "invalid_input" { 2 } else { 1 })
        }
    }
}

/// M9-AC-05: every `ancilo …` command in the documentation is accepted by
/// this parser (placeholders like `<model>` count as values).
#[cfg(test)]
mod docs_tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};
    use std::path::{Path, PathBuf};

    struct Found {
        file: String,
        line: usize,
        text: String,
        /// In a code block: a complete command. Inline: may name a command only.
        complete: bool,
    }

    fn docs() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = vec![root.join("README.md")];
        for e in std::fs::read_dir(root.join("docs")).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "md") {
                files.push(p);
            }
        }
        files.into_iter().filter(|p| p.exists()).collect()
    }

    fn found() -> Vec<Found> {
        let mut out = Vec::new();
        for file in docs() {
            let text = std::fs::read_to_string(&file).unwrap();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            let mut fence: Option<String> = None;
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("```") {
                    fence = if fence.is_some() {
                        None
                    } else {
                        Some(rest.to_string())
                    };
                    continue;
                }
                match &fence {
                    Some(lang) if lang == "bash" || lang == "sh" || lang.is_empty() => {
                        if t.starts_with("ancilo ") || t == "ancilo" {
                            out.push(Found {
                                file: name.clone(),
                                line: i + 1,
                                text: t.to_string(),
                                complete: true,
                            });
                        }
                    }
                    Some(_) => {}
                    None => {
                        for (j, part) in line.split('`').enumerate() {
                            if j % 2 == 1 && part.starts_with("ancilo ") {
                                out.push(Found {
                                    file: name.clone(),
                                    line: i + 1,
                                    text: part.to_string(),
                                    complete: false,
                                });
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Shell-like split: quotes, `# comments`, `$(…)` and placeholders.
    fn words(cmd: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut quote: Option<char> = None;
        let mut has = false;
        let mut chars = cmd.chars().peekable();
        while let Some(c) = chars.next() {
            match (quote, c) {
                (None | Some('"'), '$') if chars.peek() == Some(&'(') => {
                    let mut depth = 0;
                    for d in chars.by_ref() {
                        if d == '(' {
                            depth += 1;
                        } else if d == ')' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                    cur.push('x');
                }
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => cur.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    has = true;
                }
                (None, '#') if !has && cur.is_empty() => break,
                (None, ' ') => {
                    if has || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                (None, c) => cur.push(c),
            }
        }
        if has || !cur.is_empty() {
            out.push(cur);
        }
        out.into_iter()
            .map(|w| {
                // `[optional]` as given; `a|b` alternatives: the first;
                // `<placeholder>` / `…`: a value.
                let w = w.trim_start_matches('[').trim_end_matches(']');
                let w = w.split('|').next().unwrap_or_default().to_string();
                if w.starts_with('<') || w == "…" || w.contains('…') {
                    "x".to_string()
                } else {
                    w
                }
            })
            .collect()
    }

    #[test]
    fn every_documented_command_parses() {
        let all = found();
        assert!(
            all.len() > 40,
            "only {} commands found – extraction broken?",
            all.len()
        );
        let mut failed = Vec::new();
        for f in &all {
            let w = words(&f.text);
            if f.complete {
                if let Err(e) = Cli::try_parse_from(&w) {
                    failed.push(format!("{}:{} `{}`: {}", f.file, f.line, f.text, e.kind()));
                }
            } else {
                // Inline mention: the (sub)command path must exist.
                let mut cmd = Cli::command();
                for word in w
                    .iter()
                    .skip(1)
                    .take_while(|x| !x.starts_with('-') && *x != "x")
                {
                    match cmd.find_subcommand(word) {
                        Some(sub) => cmd = sub.clone(),
                        None => {
                            if cmd.get_subcommands().next().is_some()
                                && cmd.get_positionals().next().is_none()
                            {
                                failed.push(format!(
                                    "{}:{} `{}`: no command '{word}'",
                                    f.file, f.line, f.text
                                ));
                            }
                            break;
                        }
                    }
                }
            }
        }
        assert!(
            failed.is_empty(),
            "documented commands the CLI does not accept:\n{}",
            failed.join("\n")
        );
    }

    #[test]
    fn the_splitter_handles_quotes_placeholders_and_comments() {
        assert_eq!(
            words(r#"ancilo op x '{"a": "b c"}' --confirm  # note"#),
            ["ancilo", "op", "x", r#"{"a": "b c"}"#, "--confirm"]
        );
        assert_eq!(
            words(r#"claude --plugin-dir "$(ancilo claude-plugin)""#),
            ["claude", "--plugin-dir", "x"]
        );
        assert_eq!(
            words("ancilo disconnect claude|codex"),
            ["ancilo", "disconnect", "claude"]
        );
        assert_eq!(words("ancilo remove <model>"), ["ancilo", "remove", "x"]);
        assert_eq!(
            words("ancilo leaderboard [--kind tests]"),
            ["ancilo", "leaderboard", "--kind", "tests"]
        );
    }
}
