//! Ancilo never makes the computer unusable: models load only into memory
//! that is free, idle models are unloaded, Ancilo backs off when memory gets
//! short – and nothing is preloaded at login unless asked for.

use std::path::PathBuf;
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::{GIB, HardwareProfile};
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::{Value, json};

struct Env {
    home: TestHome,
    _hf: FakeHf,
    config: Config,
    probe: PathBuf,
    args: PathBuf,
    d: Option<DaemonHandle>,
}

fn options() -> DaemonOptions {
    DaemonOptions {
        manager: ManagerOptions {
            measure_speed: false,
            guard_interval: Duration::from_millis(100),
            ..Default::default()
        },
        ..Default::default()
    }
}

impl Env {
    async fn start() -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(vec![
            FakeRepo::new(
                "o/A-GGUF",
                vec![FakeFile::gguf("A-Q8_0.gguf", "qwen3", 4096, 100_000)],
            ),
            FakeRepo::new(
                "o/B-GGUF",
                vec![FakeFile::gguf("B-Q8_0.gguf", "qwen3", 4096, 100_000)],
            ),
        ])
        .await;
        let hw = home.scratch("hw").join("hw.json");
        std::fs::write(
            &hw,
            serde_json::to_string(&HardwareProfile::apple(16)).unwrap(),
        )
        .unwrap();
        let probe = home.scratch("probe").join("state.json");
        let args = home.scratch("args").join("args.txt");
        let config = Config {
            port: 0,
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw),
            system_probe_override: Some(probe.clone()),
            llama_server_env: [("FAKE_LLM_ARGS_FILE".to_string(), args.display().to_string())]
                .into_iter()
                .collect(),
            ..home.config()
        };
        let mut env = Self {
            home,
            _hf: hf,
            config,
            probe,
            args,
            d: None,
        };
        env.computer(12, "normal", "nominal");
        env.d = Some(
            ancilo_daemon::start(env.home.paths.clone(), env.config.clone(), options())
                .await
                .unwrap(),
        );
        env
    }

    /// What the computer reports: free memory, memory pressure, heat.
    fn computer(&self, free_gib: u64, pressure: &str, thermal: &str) {
        std::fs::write(
            &self.probe,
            json!({"available_bytes": free_gib * GIB, "pressure": pressure, "thermal": thermal})
                .to_string(),
        )
        .unwrap();
    }

    async fn call(&self, name: &str, input: Value) -> (bool, Value) {
        let d = self.d.as_ref().unwrap();
        let r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", d.url()))
            .bearer_auth(&d.token)
            .header("x-ancilo-confirm", "true")
            .json(&input)
            .send()
            .await
            .unwrap();
        (
            r.status().is_success(),
            r.json().await.unwrap_or(Value::Null),
        )
    }

    async fn op(&self, name: &str, input: Value) -> Value {
        let (ok, v) = self.call(name, input).await;
        assert!(ok, "{name}: {v}");
        v
    }

    async fn add(&self, address: &str) -> String {
        let v = self
            .op(
                "add_model",
                json!({"address": address, "start": false, "context": "small"}),
            )
            .await;
        let id = v["id"].as_str().unwrap().to_string();
        self.until(&format!("{id} ready"), || async {
            self.op("model_status", json!({"model": id})).await["status"] == "ready"
        })
        .await;
        id
    }

    async fn status(&self, id: &str) -> String {
        self.op("model_status", json!({"model": id})).await["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn until<F, Fut>(&self, what: &str, f: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..400 {
            if f().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out: {what}");
    }

    async fn restart(&mut self) {
        self.d.take().unwrap().stop().await;
        self.d = Some(
            ancilo_daemon::start(self.home.paths.clone(), self.config.clone(), options())
                .await
                .unwrap(),
        );
    }

    async fn stop(mut self) {
        self.d.take().unwrap().stop().await;
    }
}

// covers: M1-AC-14
/// A model is loaded only into memory the computer has free right now; the
/// explicit maximum takes the risk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn models_load_only_into_free_memory() {
    let env = Env::start().await;
    let a = env.add("o/A-GGUF").await;
    // Other programs leave almost nothing: refused, with a plain reason.
    env.computer(0, "normal", "nominal");
    let (ok, err) = env.call("start_model", json!({"model": a})).await;
    assert!(!ok);
    let msg = err["error"]["message"].as_str().unwrap();
    assert!(msg.contains("close some programs"), "{msg}");
    assert_eq!(err["error"]["code"], "insufficient_resources");
    assert_eq!(env.status(&a).await, "ready");
    // Critical memory pressure: nothing new either.
    env.computer(12, "critical", "nominal");
    let (ok, _) = env.call("start_model", json!({"model": a})).await;
    assert!(!ok);
    // Enough free: it runs – at low priority, with the level's parallelism.
    env.computer(12, "normal", "nominal");
    env.op("start_model", json!({"model": a})).await;
    env.until("A running", || async { env.status(&a).await == "running" })
        .await;
    let args = std::fs::read_to_string(&env.args).unwrap();
    assert!(args.contains("--parallel\n2"), "{args}");
    env.op("stop_model", json!({"model": a})).await;
    // The maximum: loaded although memory is short.
    env.op("set_resources", json!({"level": "max"})).await;
    env.computer(0, "warn", "nominal");
    env.op("start_model", json!({"model": a})).await;
    env.until("A running", || async { env.status(&a).await == "running" })
        .await;
    env.stop().await;
}

// covers: M1-AC-14
/// Idle models are unloaded after the set time; when memory gets short they
/// go at once – and come back on the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_models_give_their_memory_back() {
    let env = Env::start().await;
    let a = env.add("o/A-GGUF").await;
    let b = env.add("o/B-GGUF").await;
    let mut events = env.d.as_ref().unwrap().bus.subscribe();
    env.op("set_resources", json!({"keep_loaded_secs": 2}))
        .await;
    env.op("start_model", json!({"model": a})).await;
    env.until("A running", || async { env.status(&a).await == "running" })
        .await;
    let s = env.op("resource_status", json!({})).await;
    assert_eq!(s["settings"]["custom"], true);
    assert_eq!(s["loaded"][0]["id"], a.as_str());
    assert!(s["loaded"][0]["unload_in_secs"].as_u64().unwrap() <= 2);
    env.until("A unloaded when idle", || async {
        env.status(&a).await == "ready"
    })
    .await;
    // Memory gets short: B goes at once (well before its idle time).
    env.op("set_resources", json!({"level": "balanced"})).await;
    env.op("start_model", json!({"model": b})).await;
    env.until("B running", || async { env.status(&b).await == "running" })
        .await;
    env.computer(12, "warn", "nominal");
    env.until("B unloaded", || async { env.status(&b).await == "ready" })
        .await;
    // The status changes as the process stops; the event follows right after.
    let mut reasons = Vec::new();
    for _ in 0..200 {
        while let Ok(e) = events.try_recv() {
            if e.kind == "model.unloaded" {
                reasons.push((e.subject.unwrap_or_default(), e.data["reason"].clone()));
            }
        }
        if reasons.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        reasons,
        vec![(a.clone(), json!("idle")), (b.clone(), json!("memory"))]
    );
    let recent = env.op("resource_status", json!({})).await["recent"].clone();
    assert_eq!(recent[0]["model"], b.as_str());
    assert_eq!(recent[0]["reason"], "memory");
    // A request loads the model again.
    env.computer(12, "normal", "nominal");
    let d = env.d.as_ref().unwrap();
    let r = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", d.url()))
        .bearer_auth(&d.token)
        .json(&json!({"model": a, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    assert_eq!(env.status(&a).await, "running");
    // Unload everything with one call.
    let gone = env.op("unload_models", json!({})).await;
    assert_eq!(gone["models"], json!([a.clone()]));
    env.stop().await;
}

/// Nothing is preloaded at login unless models are meant to stay loaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn models_are_not_preloaded_at_login_unless_kept_loaded() {
    let mut env = Env::start().await;
    let a = env.add("o/A-GGUF").await;
    env.op("start_model", json!({"model": a})).await;
    env.until("A running", || async { env.status(&a).await == "running" })
        .await;
    env.restart().await;
    assert_eq!(env.status(&a).await, "ready", "balanced: loads on demand");
    env.op("set_resources", json!({"level": "max"})).await;
    env.op("start_model", json!({"model": a})).await;
    env.restart().await;
    env.until("A preloaded", || async {
        env.status(&a).await == "running"
    })
    .await;
    // The level survives the restart.
    let s = env.op("resource_status", json!({})).await;
    assert_eq!(s["settings"]["level"], "max");
    assert_eq!(s["presets"].as_array().unwrap().len(), 4);
    assert_eq!(s["total_bytes"], 16 * GIB);
    // 90 % of 16 GiB, within the GPU's share.
    assert!(s["cap_bytes"].as_u64().unwrap() <= 16 * GIB * 9 / 10);
    let (ok, _) = env.call("set_resources", json!({"max_share": 0.99})).await;
    assert!(!ok, "out of range values are refused");
    env.stop().await;
}

// covers: M1-AC-15
/// The system monitor watches the whole computer: it says when it gets
/// tight, who causes it and what helps; in an emergency Ancilo acts itself
/// (unless the user chose the maximum).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_system_monitor_names_the_cause_offers_fixes_and_acts_in_an_emergency() {
    let env = Env::start().await;
    let a = env.add("o/A-GGUF").await;
    // "Performance" only warns when memory gets tight – the fixes are the user's.
    env.op("set_resources", json!({"level": "performance"}))
        .await;
    env.op("start_model", json!({"model": a})).await;
    env.until("A running", || async { env.status(&a).await == "running" })
        .await;
    let calm = env.op("system_health", json!({})).await;
    assert_eq!(calm["level"], "ok");
    assert_eq!(calm["consumers"], json!([]));
    assert_eq!(calm["fixes"], json!([]));

    let mut events = env.d.as_ref().unwrap().bus.subscribe();
    let programs = json!([
        {"name": "Google Chrome", "memory_bytes": 8 * GIB, "cpu_percent": 12.0, "ancilo": false},
        {"name": "Simulator", "memory_bytes": 4 * GIB, "cpu_percent": 3.0, "ancilo": false},
        {"name": "Ancilo", "memory_bytes": GIB, "cpu_percent": 1.0, "ancilo": true}
    ]);
    std::fs::write(
        &env.probe,
        json!({"available_bytes": GIB / 2, "pressure": "warn", "thermal": "nominal", "programs": programs})
            .to_string(),
    )
    .unwrap();
    let h = env.op("system_health", json!({})).await;
    assert_eq!(h["level"], "tight");
    assert_eq!(h["causes"], json!(["memory"]));
    assert_eq!(h["memory_percent"], 97);
    assert_eq!(h["mostly_others"], true);
    let named: Vec<&str> = h["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(named, ["Google Chrome", "Simulator"]);
    let fixes: Vec<&str> = h["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    if cfg!(target_os = "macos") {
        assert_eq!(fixes, ["activity_monitor", "unload_models", "level_eco"]);
        // In tests nothing is opened.
        assert_eq!(
            env.op("open_activity_monitor", json!({})).await["opened"],
            false
        );
    } else {
        assert_eq!(fixes, ["unload_models", "level_eco"]);
    }
    // The guard tells the app at once – and leaves the model alone on this level.
    let mut told = false;
    for _ in 0..200 {
        while let Ok(e) = events.try_recv() {
            told |= e.kind == "system.health" && e.data["level"] == "tight";
        }
        if told {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(told, "the guard reports the new verdict");
    assert_eq!(env.status(&a).await, "running");
    // It burns: an emergency – Ancilo unloads its model itself.
    std::fs::write(
        &env.probe,
        json!({"available_bytes": 8 * GIB, "pressure": "normal", "thermal": "critical", "programs": programs})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        env.op("system_health", json!({})).await["level"],
        "critical"
    );
    env.until("A unloaded in the emergency", || async {
        env.status(&a).await == "ready"
    })
    .await;
    // The guard notes it right after the model has stopped.
    env.until("noted as heat", || async {
        env.op("resource_status", json!({})).await["recent"][0]["reason"] == "heat"
    })
    .await;
    env.stop().await;
}
