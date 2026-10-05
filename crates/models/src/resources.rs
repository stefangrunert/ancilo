//! How much of the computer Ancilo may take – and the guarantees that it
//! never makes it unusable: no model is loaded into memory the system does
//! not have free, idle models are unloaded, model processes run at a lower
//! priority, and Ancilo backs off when memory gets short or the machine hot.
//!
//! One setting with four levels (the app's slider); each level is a preset
//! of the individual values, which can also be set one by one.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hardware::GIB;

/// Kept free on top of what a model needs, so the system never has to swap.
pub const SAFETY_BYTES: u64 = GIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Ancilo holds back: the computer's other work always comes first.
    Eco,
    /// The default: quick answers, memory given back after a break.
    Balanced,
    /// Stays ready longer and uses more precise variants.
    Performance,
    /// Everything for speed; other programs may slow down.
    Max,
}

/// Priority of model processes against the computer's other programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// macOS background QoS (efficiency cores; ≈ 25 % slower).
    Background,
    /// macOS utility QoS (≈ 4 % slower; the system prefers other programs).
    Low,
    Normal,
}

/// Which variant of a model to prefer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    /// The smallest (Q4): half the memory, little less quality.
    Small,
    /// The one that runs best here.
    Auto,
    /// The most precise that fits comfortably.
    Precise,
}

/// What Ancilo does when memory gets short or the computer hot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Guard {
    /// Unload idle models as soon as memory gets tight or the computer warm.
    Unload,
    /// Unload idle models when memory is short or the computer hot.
    UnloadWhenTight,
    /// Only report it.
    Warn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceSettings {
    pub level: Level,
    /// Values were changed one by one (not exactly the level's preset).
    #[serde(default)]
    pub custom: bool,
    /// Unload a model after this long without use (`None`: keep it loaded).
    pub keep_loaded_secs: Option<u64>,
    /// Share of the computer's memory Ancilo's models may use at most.
    pub max_share: f64,
    /// Requests one model works on at the same time.
    pub parallel: u32,
    pub priority: Priority,
    pub variant: Variant,
    pub guard: Guard,
}

impl ResourceSettings {
    pub fn preset(level: Level) -> Self {
        let (keep, share, parallel, priority, variant, guard) = match level {
            Level::Eco => (
                Some(5 * 60),
                0.35,
                1,
                Priority::Background,
                Variant::Small,
                Guard::Unload,
            ),
            Level::Balanced => (
                Some(15 * 60),
                0.55,
                2,
                Priority::Low,
                Variant::Auto,
                Guard::UnloadWhenTight,
            ),
            Level::Performance => (
                Some(60 * 60),
                0.70,
                3,
                Priority::Normal,
                Variant::Precise,
                Guard::Warn,
            ),
            Level::Max => (
                None,
                0.90,
                4,
                Priority::Normal,
                Variant::Precise,
                Guard::Warn,
            ),
        };
        Self {
            level,
            custom: false,
            keep_loaded_secs: keep,
            max_share: share,
            parallel,
            priority,
            variant,
            guard,
        }
    }

    /// Checks values set one by one.
    pub fn validate(&self) -> Result<(), String> {
        if !(0.1..=0.95).contains(&self.max_share) {
            return Err("the memory share must be between 10 % and 95 %".into());
        }
        if !(1..=8).contains(&self.parallel) {
            return Err("parallel requests must be between 1 and 8".into());
        }
        if self.keep_loaded_secs.is_some_and(|s| s < 2) {
            return Err("keep models loaded for at least 2 seconds".into());
        }
        Ok(())
    }

    /// Ancilo may load models at all without checking what is free right
    /// now (only at the explicit maximum).
    pub fn protects(&self) -> bool {
        self.level != Level::Max || self.custom
    }

    /// Restart models after a daemon restart (at login) – only when they are
    /// meant to stay loaded anyway.
    pub fn preload(&self) -> bool {
        self.keep_loaded_secs.is_none()
    }
}

impl Default for ResourceSettings {
    fn default() -> Self {
        Self::preset(Level::Balanced)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    Unknown,
    Normal,
    Warn,
    Critical,
}

/// macOS thermal pressure (`com.apple.system.thermalpressurelevel`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Thermal {
    Unknown,
    Nominal,
    Moderate,
    Heavy,
    Critical,
}

/// The computer right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SystemState {
    /// Memory other programs leave free (None: unknown).
    pub available_bytes: Option<u64>,
    pub pressure: Pressure,
    pub thermal: Thermal,
    /// Swap in use (None: unknown).
    #[serde(default)]
    pub swap_used_bytes: Option<u64>,
    /// Processor load of the whole computer now, 0–100 (None: not measured yet).
    #[serde(default)]
    pub cpu_percent: Option<f32>,
    /// … and on average over the last half minute.
    #[serde(default)]
    pub cpu_sustained_percent: Option<f32>,
}

impl SystemState {
    pub fn unknown() -> Self {
        Self {
            available_bytes: None,
            pressure: Pressure::Unknown,
            thermal: Thermal::Unknown,
            swap_used_bytes: None,
            cpu_percent: None,
            cpu_sustained_percent: None,
        }
    }
}

/// Reads the computer's state; `override_file` (tests) replaces it.
pub fn probe(override_file: Option<&Path>) -> SystemState {
    if let Some(path) = override_file {
        return std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(SystemState::unknown);
    }
    SystemState {
        available_bytes: crate::hardware::available_memory_now(),
        pressure: os::pressure(),
        thermal: os::thermal(),
        swap_used_bytes: os::swap_used(),
        cpu_percent: None,
        cpu_sustained_percent: None,
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{Pressure, Thermal};
    use std::process::Command;

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn pressure() -> Pressure {
        match run(
            "/usr/sbin/sysctl",
            &["-n", "kern.memorystatus_vm_pressure_level"],
        )
        .as_deref()
        {
            Some("1") => Pressure::Normal,
            Some("2") => Pressure::Warn,
            Some("4") => Pressure::Critical,
            _ => Pressure::Unknown,
        }
    }

    pub fn thermal() -> Thermal {
        // "com.apple.system.thermalpressurelevel 0"
        let level = run(
            "/usr/bin/notifyutil",
            &["-g", "com.apple.system.thermalpressurelevel"],
        )
        .and_then(|s| s.split_whitespace().last()?.parse::<u32>().ok());
        match level {
            Some(0) => Thermal::Nominal,
            Some(1) => Thermal::Moderate,
            Some(2) => Thermal::Heavy,
            Some(_) => Thermal::Critical,
            None => Thermal::Unknown,
        }
    }

    pub fn swap_used() -> Option<u64> {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        Some(sys.used_swap())
    }
}

#[cfg(not(target_os = "macos"))]
mod os {
    use super::{Pressure, Thermal};

    /// Linux pressure stall information: share of time tasks waited for memory.
    pub fn pressure() -> Pressure {
        let Ok(text) = std::fs::read_to_string("/proc/pressure/memory") else {
            return Pressure::Unknown;
        };
        let avg10 = text
            .lines()
            .find(|l| l.starts_with("some"))
            .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10=")))
            .and_then(|v| v.parse::<f64>().ok());
        match avg10 {
            Some(v) if v >= 20.0 => Pressure::Critical,
            Some(v) if v >= 5.0 => Pressure::Warn,
            Some(_) => Pressure::Normal,
            None => Pressure::Unknown,
        }
    }

    pub fn thermal() -> Thermal {
        Thermal::Unknown
    }

    pub fn swap_used() -> Option<u64> {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        Some(sys.used_swap())
    }
}

/// May a model that needs `need` bytes be loaded now? `reclaimable`: what
/// Ancilo's own idle models would free.
pub fn admit(
    settings: &ResourceSettings,
    state: &SystemState,
    need: u64,
    reclaimable: u64,
) -> Result<(), String> {
    if !settings.protects() {
        return Ok(());
    }
    if state.pressure == Pressure::Critical {
        return Err(ancilo_core::msg("memory.short", &[]));
    }
    if let Some(free) = state.available_bytes {
        let room = (free + reclaimable).saturating_sub(SAFETY_BYTES);
        if need > room {
            return Err(ancilo_core::msg(
                "memory.need",
                &[("need", &gb(need)), ("room", &gb(room))],
            ));
        }
    }
    Ok(())
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GIB as f64)
}

/// A loaded model, as the guard sees it.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub id: String,
    /// Answering a request right now.
    pub busy: bool,
    /// An agent is working with it (between its requests, too): only an
    /// emergency unloads it.
    pub working: bool,
    pub pinned: bool,
    pub idle_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Not used for a while.
    Idle,
    /// Memory got short.
    Memory,
    /// The computer got hot.
    Heat,
}

/// Which models to unload now, and why. Never a model that is answering;
/// pinned models only when memory is critical.
pub fn to_unload(
    settings: &ResourceSettings,
    state: &SystemState,
    loaded: &[Loaded],
) -> Vec<(String, Reason)> {
    let memory = match settings.guard {
        Guard::Unload => state.pressure >= Pressure::Warn,
        Guard::UnloadWhenTight => state.pressure >= Pressure::Warn,
        Guard::Warn => false,
    };
    let heat = match settings.guard {
        Guard::Unload => state.thermal >= Thermal::Moderate,
        Guard::UnloadWhenTight => state.thermal >= Thermal::Heavy,
        Guard::Warn => false,
    };
    // An emergency: Ancilo acts on every level but the explicit maximum.
    let critical = state.pressure == Pressure::Critical && settings.protects();
    let burning = state.thermal == Thermal::Critical && settings.protects();
    loaded
        .iter()
        .filter(|m| !m.busy)
        .filter(|m| !m.working || critical || burning)
        .filter_map(|m| {
            let idle = settings.keep_loaded_secs.is_some_and(|k| m.idle_secs >= k);
            let reason = if critical || (memory && !m.pinned) {
                Some(Reason::Memory)
            } else if burning || (heat && !m.pinned) {
                Some(Reason::Heat)
            } else if idle && !m.pinned {
                Some(Reason::Idle)
            } else {
                None
            };
            reason.map(|r| (m.id.clone(), r))
        })
        .collect()
}

/// Seconds until an idle model is unloaded (None: it stays).
pub fn unload_in(settings: &ResourceSettings, m: &Loaded) -> Option<u64> {
    if m.pinned || m.busy || m.working {
        return None;
    }
    settings
        .keep_loaded_secs
        .map(|k| k.saturating_sub(m.idle_secs))
}

/// The command that starts a model process at this priority.
pub fn priority_command(priority: Priority, program: &Path) -> (std::path::PathBuf, Vec<String>) {
    let tool = if cfg!(target_os = "macos") {
        match priority {
            Priority::Background => Some(("/usr/sbin/taskpolicy", ["-c", "background"])),
            Priority::Low => Some(("/usr/sbin/taskpolicy", ["-c", "utility"])),
            Priority::Normal => None,
        }
    } else {
        match priority {
            Priority::Background => Some(("nice", ["-n", "19"])),
            Priority::Low => Some(("nice", ["-n", "10"])),
            Priority::Normal => None,
        }
    };
    match tool {
        Some((tool, args)) if cfg!(target_os = "macos") && !Path::new(tool).exists() => {
            let _ = args;
            (program.to_path_buf(), Vec::new())
        }
        Some((tool, args)) => (
            tool.into(),
            args.iter()
                .map(|a| a.to_string())
                .chain([program.display().to_string()])
                .collect(),
        ),
        None => (program.to_path_buf(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(free_gib: Option<u64>, pressure: Pressure, thermal: Thermal) -> SystemState {
        SystemState {
            available_bytes: free_gib.map(|g| g * GIB),
            pressure,
            thermal,
            swap_used_bytes: None,
            cpu_percent: None,
            cpu_sustained_percent: None,
        }
    }

    fn model(id: &str, busy: bool, pinned: bool, idle_secs: u64) -> Loaded {
        Loaded {
            id: id.into(),
            busy,
            working: false,
            pinned,
            idle_secs,
        }
    }

    #[test]
    fn levels_go_from_restraint_to_speed() {
        let levels = [Level::Eco, Level::Balanced, Level::Performance, Level::Max];
        let p: Vec<ResourceSettings> = levels
            .iter()
            .map(|l| ResourceSettings::preset(*l))
            .collect();
        for w in p.windows(2) {
            assert!(w[0].max_share < w[1].max_share);
            assert!(w[0].parallel < w[1].parallel);
            let keep = |s: &ResourceSettings| s.keep_loaded_secs.unwrap_or(u64::MAX);
            assert!(keep(&w[0]) < keep(&w[1]));
        }
        assert_eq!(ResourceSettings::default().level, Level::Balanced);
        assert_eq!(ResourceSettings::default().priority, Priority::Low);
        assert!(p.iter().all(|s| s.validate().is_ok()));
        // Only "keep loaded" means "load again at login".
        assert!(!ResourceSettings::default().preload());
        assert!(ResourceSettings::preset(Level::Max).preload());
    }

    // covers: M1-AC-14
    /// Never into swap: a model loads only into memory that is free right
    /// now (what other programs leave, plus what Ancilo's idle models free).
    #[test]
    fn a_model_loads_only_into_free_memory() {
        let s = ResourceSettings::default();
        let need = 6 * GIB;
        assert!(
            admit(
                &s,
                &state(Some(16), Pressure::Normal, Thermal::Nominal),
                need,
                0
            )
            .is_ok()
        );
        let refused = admit(
            &s,
            &state(Some(4), Pressure::Normal, Thermal::Nominal),
            need,
            0,
        )
        .unwrap_err();
        assert!(refused.contains("close some programs"), "{refused}");
        // Unloading Ancilo's own idle model makes room.
        assert!(
            admit(
                &s,
                &state(Some(4), Pressure::Normal, Thermal::Nominal),
                need,
                4 * GIB
            )
            .is_ok()
        );
        // The safety margin counts.
        assert!(
            admit(
                &s,
                &state(Some(6), Pressure::Normal, Thermal::Nominal),
                need,
                0
            )
            .is_err()
        );
        // Critical pressure: nothing new.
        assert!(
            admit(
                &s,
                &state(Some(64), Pressure::Critical, Thermal::Nominal),
                GIB,
                0
            )
            .is_err()
        );
        // Unknown free memory: the budget alone decides (checked elsewhere).
        assert!(
            admit(
                &s,
                &state(None, Pressure::Unknown, Thermal::Unknown),
                need,
                0
            )
            .is_ok()
        );
        // The explicit maximum takes the risk.
        let max = ResourceSettings::preset(Level::Max);
        assert!(
            admit(
                &max,
                &state(Some(2), Pressure::Critical, Thermal::Nominal),
                need,
                0
            )
            .is_ok()
        );
    }

    // covers: M1-AC-14
    #[test]
    fn the_model_of_an_agent_at_work_goes_only_in_an_emergency() {
        let s = ResourceSettings::default();
        let working = Loaded {
            working: true,
            ..model("agent", false, false, 99 * 60)
        };
        let loaded = [working.clone()];
        // Long idle between its requests, memory tight, the Mac warm: it stays.
        for st in [
            state(Some(32), Pressure::Normal, Thermal::Nominal),
            state(Some(1), Pressure::Warn, Thermal::Nominal),
            state(Some(32), Pressure::Normal, Thermal::Heavy),
        ] {
            assert_eq!(to_unload(&s, &st, &loaded), vec![], "{st:?}");
        }
        assert_eq!(unload_in(&s, &working), None);
        // An emergency still unloads it: the computer comes first.
        let critical = state(Some(1), Pressure::Critical, Thermal::Nominal);
        assert_eq!(
            to_unload(&s, &critical, &loaded),
            vec![("agent".into(), Reason::Memory)]
        );
    }

    #[test]
    fn idle_hot_or_short_models_are_unloaded_but_never_while_answering() {
        let s = ResourceSettings::default();
        let calm = state(Some(32), Pressure::Normal, Thermal::Nominal);
        let loaded = [
            model("fresh", false, false, 60),
            model("idle", false, false, 16 * 60),
            model("busy", true, false, 99 * 60),
            model("pinned", false, true, 99 * 60),
        ];
        assert_eq!(
            to_unload(&s, &calm, &loaded),
            vec![("idle".into(), Reason::Idle)]
        );
        // Memory gets short: every idle, unpinned model goes.
        let short = state(Some(1), Pressure::Warn, Thermal::Nominal);
        assert_eq!(
            to_unload(&s, &short, &loaded),
            vec![
                ("fresh".into(), Reason::Memory),
                ("idle".into(), Reason::Memory)
            ]
        );
        // Critical: pinned ones too – but never the one answering.
        let critical = state(Some(1), Pressure::Critical, Thermal::Nominal);
        let ids: Vec<String> = to_unload(&s, &critical, &loaded)
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(ids, ["fresh", "idle", "pinned"]);
        // Heat: balanced reacts at "heavy", eco already at "moderate".
        let warm = state(Some(32), Pressure::Normal, Thermal::Moderate);
        assert_eq!(to_unload(&s, &warm, &loaded[..1]), vec![]);
        let eco = ResourceSettings::preset(Level::Eco);
        assert_eq!(
            to_unload(&eco, &warm, &loaded[..1]),
            vec![("fresh".into(), Reason::Heat)]
        );
        let hot = state(Some(32), Pressure::Normal, Thermal::Heavy);
        assert_eq!(
            to_unload(&s, &hot, &loaded[..1]),
            vec![("fresh".into(), Reason::Heat)]
        );
        // The maximum keeps everything.
        let max = ResourceSettings::preset(Level::Max);
        assert!(to_unload(&max, &short, &loaded).is_empty());
        assert_eq!(unload_in(&s, &loaded[0]), Some(14 * 60));
        assert_eq!(unload_in(&max, &loaded[0]), None);
    }

    // covers: M1-AC-15
    #[test]
    fn an_emergency_acts_on_every_level_but_the_maximum() {
        let loaded = [model("big", false, true, 0), model("busy", true, false, 0)];
        let hot = state(Some(20), Pressure::Normal, Thermal::Critical);
        // "Performance" only warns when it gets tight – but not when it burns.
        let perf = ResourceSettings::preset(Level::Performance);
        assert_eq!(
            to_unload(&perf, &hot, &loaded),
            vec![("big".to_string(), Reason::Heat)],
            "never the one answering"
        );
        let full = state(Some(0), Pressure::Critical, Thermal::Nominal);
        assert_eq!(
            to_unload(&perf, &full, &loaded),
            vec![("big".to_string(), Reason::Memory)]
        );
        // The explicit maximum: the user chose to only be warned.
        let max = ResourceSettings::preset(Level::Max);
        assert!(to_unload(&max, &hot, &loaded).is_empty());
        assert!(to_unload(&max, &full, &loaded).is_empty());
    }

    #[test]
    fn values_set_one_by_one_are_checked() {
        let mut s = ResourceSettings {
            max_share: 0.99,
            ..ResourceSettings::default()
        };
        assert!(s.validate().is_err());
        s.max_share = 0.5;
        s.parallel = 0;
        assert!(s.validate().is_err());
        s.parallel = 2;
        s.keep_loaded_secs = Some(1);
        assert!(s.validate().is_err());
    }

    #[test]
    fn lower_priority_wraps_the_command() {
        let (cmd, args) = priority_command(Priority::Normal, Path::new("/x/llama-server"));
        assert_eq!(cmd, Path::new("/x/llama-server"));
        assert!(args.is_empty());
        let (cmd, args) = priority_command(Priority::Low, Path::new("/x/llama-server"));
        if cfg!(target_os = "macos") {
            assert_eq!(cmd, Path::new("/usr/sbin/taskpolicy"));
            assert_eq!(args, ["-c", "utility", "/x/llama-server"]);
        } else {
            assert_eq!(args, ["-n", "10", "/x/llama-server"]);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_reports_its_state() {
        let s = probe(None);
        assert!(s.available_bytes.is_some());
        assert_ne!(s.pressure, Pressure::Unknown);
        assert_ne!(s.thermal, Thermal::Unknown);
        assert!(s.swap_used_bytes.is_some());
    }
}
