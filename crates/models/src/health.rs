//! The system monitor: is the computer about to be saturated – memory full,
//! processor overloaded, too hot – who is causing it, and what helps with one
//! click. Ancilo watches the whole computer, not only itself: a laptop that
//! grinds to a halt is Ancilo's problem even when another program is the cause
//! (decision `2026-10-02-systemmonitor`).
//!
//! Cheap by design: the processor load comes from one counter read per look;
//! the list of programs is only read while the computer is strained.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::resources::{Level, Pressure, ResourceSettings, SystemState, Thermal};

/// Processor load (whole computer, all cores) that counts as overloaded …
pub const CPU_OVERLOADED: f32 = 90.0;
/// … when it lasts this long.
pub const CPU_WINDOW: Duration = Duration::from_secs(30);
/// Free memory below this share of the total counts as short, also where the
/// system reports no memory pressure.
pub const LOW_FREE_SHARE: f64 = 0.05;
/// Programs below these are not worth naming.
const MIN_NAMED_BYTES: u64 = 300 << 20;
const MIN_NAMED_CPU: f32 = 15.0;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HealthLevel {
    /// All calm.
    Ok,
    /// Close to the limit: the computer may get slow.
    Tight,
    /// At the limit: the computer is (about to be) unusable.
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Memory,
    Cpu,
    Heat,
}

/// One click that helps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    /// Unload Ancilo's models that are not answering (`unload_models`).
    UnloadModels { frees_bytes: u64 },
    /// Set the cockpit to "eco" (`set_resources`).
    LevelEco,
    /// Open the Activity Monitor to close other programs (`open_activity_monitor`).
    ActivityMonitor,
}

/// A program and what it takes; Ancilo's own processes count as one ("Ancilo").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Consumer {
    pub name: String,
    pub memory_bytes: u64,
    /// Share of the whole computer's processor (0–100).
    pub cpu_percent: f32,
    /// One of Ancilo's own processes (daemon, models).
    pub ancilo: bool,
}

/// The verdict on the computer right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Health {
    pub level: HealthLevel,
    /// What makes it tight, most pressing first.
    pub causes: Vec<Cause>,
    /// Memory in use by everything (0–100; None: unknown).
    pub memory_percent: Option<u8>,
    /// Processor load now (0–100; None: not measured yet).
    pub cpu_percent: Option<u8>,
    pub pressure: Pressure,
    pub thermal: Thermal,
    /// Memory Ancilo's models take.
    pub ancilo_bytes: u64,
    /// Whether other programs, not Ancilo, take most of what is short.
    pub mostly_others: bool,
    /// The biggest other programs (only while it is tight; empty otherwise).
    pub consumers: Vec<Consumer>,
    /// What helps, best first.
    pub fixes: Vec<Fix>,
}

/// What Ancilo has loaded, as the assessment needs it.
#[derive(Debug, Clone, Default)]
pub struct Own {
    /// Memory of all loaded models.
    pub bytes: u64,
    /// Memory of the models that are not answering (what unloading frees).
    pub idle_bytes: u64,
    /// Processor share of Ancilo's processes.
    pub cpu_percent: f32,
}

fn percent(v: f64) -> u8 {
    v.round().clamp(0.0, 100.0) as u8
}

/// Judges the computer's state; `consumers` are all programs (Ancilo's
/// included), `activity_monitor` whether the computer has one to open.
pub fn assess(
    state: &SystemState,
    total_bytes: u64,
    settings: &ResourceSettings,
    own: &Own,
    consumers: &[Consumer],
    activity_monitor: bool,
) -> Health {
    let memory_percent = state
        .available_bytes
        .filter(|_| total_bytes > 0)
        .map(|free| percent(100.0 * (1.0 - free.min(total_bytes) as f64 / total_bytes as f64)));
    let low_free = state
        .available_bytes
        .is_some_and(|free| (free as f64) < total_bytes as f64 * LOW_FREE_SHARE);

    let mut causes = Vec::new();
    let mut level = HealthLevel::Ok;
    let mut raise = |l: HealthLevel, c: Cause, causes: &mut Vec<Cause>| {
        level = level.max(l);
        if !causes.contains(&c) {
            causes.push(c);
        }
    };
    match state.pressure {
        Pressure::Critical => raise(HealthLevel::Critical, Cause::Memory, &mut causes),
        Pressure::Warn => raise(HealthLevel::Tight, Cause::Memory, &mut causes),
        _ if low_free => raise(HealthLevel::Tight, Cause::Memory, &mut causes),
        _ => {}
    }
    match state.thermal {
        Thermal::Critical => raise(HealthLevel::Critical, Cause::Heat, &mut causes),
        Thermal::Heavy => raise(HealthLevel::Tight, Cause::Heat, &mut causes),
        _ => {}
    }
    if state
        .cpu_sustained_percent
        .is_some_and(|c| c >= CPU_OVERLOADED)
    {
        raise(HealthLevel::Tight, Cause::Cpu, &mut causes);
    }
    // The most pressing first: memory, then heat, then the processor.
    causes.sort_by_key(|c| match c {
        Cause::Memory => 0,
        Cause::Heat => 1,
        Cause::Cpu => 2,
    });

    let others: Vec<&Consumer> = consumers.iter().filter(|c| !c.ancilo).collect();
    let others_bytes: u64 = state
        .available_bytes
        .map(|free| total_bytes.saturating_sub(free).saturating_sub(own.bytes))
        .unwrap_or_else(|| others.iter().map(|c| c.memory_bytes).sum());
    let others_cpu: f32 = others.iter().map(|c| c.cpu_percent).sum();
    let mostly_others = match causes.first() {
        Some(Cause::Memory) => others_bytes > own.bytes,
        Some(Cause::Cpu) | Some(Cause::Heat) => others_cpu > own.cpu_percent,
        None => false,
    };

    let mut named: Vec<Consumer> = Vec::new();
    if level > HealthLevel::Ok {
        let by_cpu = causes.first() == Some(&Cause::Cpu);
        let mut list: Vec<Consumer> = others
            .into_iter()
            .filter(|c| {
                if by_cpu {
                    c.cpu_percent >= MIN_NAMED_CPU
                } else {
                    c.memory_bytes >= MIN_NAMED_BYTES
                }
            })
            .cloned()
            .collect();
        if by_cpu {
            list.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
        } else {
            list.sort_by_key(|c| std::cmp::Reverse(c.memory_bytes));
        }
        list.truncate(3);
        named = list;
    }

    let mut fixes = Vec::new();
    if level > HealthLevel::Ok {
        if own.idle_bytes > 0 {
            fixes.push(Fix::UnloadModels {
                frees_bytes: own.idle_bytes,
            });
        }
        if settings.level != Level::Eco && (own.bytes > 0 || own.cpu_percent > 0.0) {
            fixes.push(Fix::LevelEco);
        }
        if activity_monitor && mostly_others {
            fixes.push(Fix::ActivityMonitor);
        }
        // Ancilo cannot help itself out of it: first what the user can do.
        if mostly_others && let Some(i) = fixes.iter().position(|f| *f == Fix::ActivityMonitor) {
            let f = fixes.remove(i);
            fixes.insert(0, f);
        }
    }

    Health {
        level,
        causes,
        memory_percent,
        cpu_percent: state.cpu_percent.map(|c| percent(c as f64)),
        pressure: state.pressure,
        thermal: state.thermal,
        ancilo_bytes: own.bytes,
        mostly_others,
        consumers: named,
        fixes,
    }
}

/// The name a process is shown under: its app (helpers count with their app),
/// or else the process's own name.
pub fn program_name(exe: Option<&Path>, name: &str) -> String {
    if let Some(exe) = exe {
        for part in exe.components() {
            let s = part.as_os_str().to_string_lossy();
            if let Some(app) = s.strip_suffix(".app") {
                return app.to_string();
            }
        }
    }
    name.to_string()
}

/// Measures the processor and lists programs; keeps what it needs between
/// looks (the processor load is the change since the last look).
pub struct Sampler {
    sys: System,
    cores: usize,
    last_cpu: Option<Instant>,
    cpu: Option<f32>,
    history: VecDeque<(Instant, f32)>,
    programs: Option<(Instant, Vec<Consumer>)>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        let cores = sys.cpus().len().max(1);
        Self {
            sys,
            cores,
            // The first look measures since this first read.
            last_cpu: None,
            cpu: None,
            history: VecDeque::new(),
            programs: None,
        }
    }

    /// Processor load now and over the last [`CPU_WINDOW`] (0–100). Looks
    /// closer together than a second reuse the last value.
    pub fn cpu(&mut self) -> (Option<f32>, Option<f32>) {
        let now = Instant::now();
        let due = self
            .last_cpu
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1));
        if due {
            self.sys.refresh_cpu_usage();
            let v = self.sys.global_cpu_usage();
            self.last_cpu = Some(now);
            self.cpu = Some(v);
            self.history.push_back((now, v));
        }
        while self
            .history
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > CPU_WINDOW)
        {
            self.history.pop_front();
        }
        // Only a judgement over (most of) the window: one busy second is no overload.
        let span = self
            .history
            .front()
            .map(|(t, _)| now.duration_since(*t))
            .unwrap_or_default();
        let sustained = (self.history.len() >= 2 && span >= CPU_WINDOW / 2)
            .then(|| self.history.iter().map(|(_, v)| *v).sum::<f32>() / self.history.len() as f32);
        (self.cpu, sustained)
    }

    /// All programs with their memory and processor share, Ancilo's processes
    /// (this one and its children) as one. Read at most every few seconds.
    pub fn programs(&mut self) -> Vec<Consumer> {
        let now = Instant::now();
        if let Some((at, list)) = &self.programs
            && now.duration_since(*at) < Duration::from_secs(5)
        {
            return list.clone();
        }
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                .with_exe(UpdateKind::OnlyIfNotSet),
        );
        let me = sysinfo::Pid::from_u32(std::process::id());
        let mut by_name: HashMap<String, Consumer> = HashMap::new();
        for (pid, p) in self.sys.processes() {
            let own = *pid == me || p.parent() == Some(me);
            let name = if own {
                "Ancilo".to_string()
            } else {
                program_name(p.exe(), &p.name().to_string_lossy())
            };
            // The app's window and a daemon started from the app are Ancilo too.
            let ancilo = own || name == "Ancilo";
            let e = by_name.entry(name.clone()).or_insert(Consumer {
                name,
                memory_bytes: 0,
                cpu_percent: 0.0,
                ancilo,
            });
            e.memory_bytes += p.memory();
            // Per process the load is per core; as a share of the whole computer:
            e.cpu_percent += p.cpu_usage() / self.cores as f32;
        }
        let list: Vec<Consumer> = by_name.into_values().collect();
        self.programs = Some((now, list.clone()));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::GIB;

    fn state(free_gib: u64, pressure: Pressure, thermal: Thermal, cpu: Option<f32>) -> SystemState {
        SystemState {
            available_bytes: Some(free_gib * GIB),
            pressure,
            thermal,
            swap_used_bytes: None,
            cpu_percent: cpu,
            cpu_sustained_percent: cpu,
        }
    }

    fn program(name: &str, gib: u64, cpu: f32) -> Consumer {
        Consumer {
            name: name.into(),
            memory_bytes: gib * GIB,
            cpu_percent: cpu,
            ancilo: name == "Ancilo",
        }
    }

    fn balanced() -> ResourceSettings {
        ResourceSettings::preset(Level::Balanced)
    }

    // covers: M1-AC-15
    #[test]
    fn calm_computer_needs_nothing() {
        let h = assess(
            &state(10, Pressure::Normal, Thermal::Nominal, Some(20.0)),
            16 * GIB,
            &balanced(),
            &Own::default(),
            &[program("Safari", 2, 5.0)],
            true,
        );
        assert_eq!(h.level, HealthLevel::Ok);
        assert!(h.causes.is_empty() && h.fixes.is_empty() && h.consumers.is_empty());
        assert_eq!(h.memory_percent, Some(38));
        assert_eq!(h.cpu_percent, Some(20));
    }

    // covers: M1-AC-15
    #[test]
    fn ancilo_filling_memory_offers_to_unload_and_eco() {
        let own = Own {
            bytes: 9 * GIB,
            idle_bytes: 9 * GIB,
            cpu_percent: 0.0,
        };
        let h = assess(
            &state(1, Pressure::Warn, Thermal::Nominal, Some(30.0)),
            16 * GIB,
            &balanced(),
            &own,
            &[program("Ancilo", 9, 1.0), program("Safari", 3, 2.0)],
            true,
        );
        assert_eq!(h.level, HealthLevel::Tight);
        assert_eq!(h.causes, vec![Cause::Memory]);
        assert!(!h.mostly_others);
        assert_eq!(
            h.fixes,
            vec![
                Fix::UnloadModels {
                    frees_bytes: 9 * GIB
                },
                Fix::LevelEco
            ]
        );
        // Ancilo is not named among the others.
        assert_eq!(
            h.consumers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Safari"]
        );
    }

    // covers: M1-AC-15
    #[test]
    fn other_programs_are_named_and_the_activity_monitor_comes_first() {
        let own = Own {
            bytes: 2 * GIB,
            idle_bytes: 2 * GIB,
            cpu_percent: 0.0,
        };
        let h = assess(
            &state(0, Pressure::Critical, Thermal::Nominal, None),
            16 * GIB,
            &balanced(),
            &own,
            &[
                program("Google Chrome", 8, 10.0),
                program("Simulator", 4, 3.0),
                program("Ancilo", 2, 0.0),
                program("Tiny", 0, 0.0),
                program("Mail", 1, 0.0),
                program("Music", 1, 0.0),
            ],
            true,
        );
        assert_eq!(h.level, HealthLevel::Critical);
        assert!(h.mostly_others);
        assert_eq!(
            h.consumers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Google Chrome", "Simulator", "Mail"]
        );
        assert_eq!(h.fixes[0], Fix::ActivityMonitor);
        // Without an Activity Monitor (Linux) only what Ancilo can do.
        let h = assess(
            &state(0, Pressure::Critical, Thermal::Nominal, None),
            16 * GIB,
            &balanced(),
            &own,
            &[program("chrome", 8, 0.0)],
            false,
        );
        assert!(!h.fixes.contains(&Fix::ActivityMonitor));
    }

    // covers: M1-AC-15
    #[test]
    fn processor_counts_only_when_overloaded_for_a_while_and_heat_by_level() {
        let busy = assess(
            &SystemState {
                cpu_sustained_percent: Some(60.0),
                ..state(8, Pressure::Normal, Thermal::Nominal, Some(100.0))
            },
            16 * GIB,
            &balanced(),
            &Own::default(),
            &[],
            true,
        );
        assert_eq!(
            busy.level,
            HealthLevel::Ok,
            "one busy moment is no overload"
        );
        let over = assess(
            &state(8, Pressure::Normal, Thermal::Moderate, Some(95.0)),
            16 * GIB,
            &balanced(),
            &Own {
                cpu_percent: 80.0,
                ..Own::default()
            },
            &[program("Ancilo", 1, 80.0), program("Xcode", 1, 15.0)],
            true,
        );
        assert_eq!(
            (over.level, over.causes.clone()),
            (HealthLevel::Tight, vec![Cause::Cpu])
        );
        assert!(!over.mostly_others);
        assert_eq!(over.fixes, vec![Fix::LevelEco]);
        let hot = assess(
            &state(8, Pressure::Normal, Thermal::Critical, Some(50.0)),
            16 * GIB,
            &balanced(),
            &Own::default(),
            &[],
            true,
        );
        assert_eq!(
            (hot.level, hot.causes),
            (HealthLevel::Critical, vec![Cause::Heat])
        );
        // Already eco and nothing loaded: nothing Ancilo could still do.
        let eco = assess(
            &state(0, Pressure::Warn, Thermal::Nominal, None),
            16 * GIB,
            &ResourceSettings::preset(Level::Eco),
            &Own::default(),
            &[],
            false,
        );
        assert!(eco.fixes.is_empty());
    }

    // covers: M1-AC-15
    #[test]
    fn helpers_count_with_their_app() {
        let p = Path::new(
            "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)",
        );
        assert_eq!(
            program_name(Some(p), "Google Chrome Helper (Renderer)"),
            "Google Chrome"
        );
        assert_eq!(
            program_name(Some(Path::new("/usr/libexec/syspolicyd")), "syspolicyd"),
            "syspolicyd"
        );
        assert_eq!(program_name(None, "kernel_task"), "kernel_task");
    }

    #[test]
    fn this_computer_is_measured_cheaply() {
        let mut s = Sampler::new();
        std::thread::sleep(Duration::from_millis(250));
        let (now, _) = s.cpu();
        let now = now.expect("processor load");
        assert!((0.0..=100.0).contains(&now));
        let t = Instant::now();
        let programs = s.programs();
        let took = t.elapsed();
        assert!(
            programs.iter().any(|p| p.ancilo && p.name == "Ancilo"),
            "this process counts as Ancilo"
        );
        assert!(programs.len() > 5);
        eprintln!("{} programs read in {took:?}", programs.len());
    }
}
