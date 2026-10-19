//! Conservative routing into Ancilo's controls. An everyday word such as
//! "role", "hot" or "slow" alone must not turn a chat into a system task.

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

fn has(words: &[String], choices: &[&str]) -> bool {
    words.iter().any(|w| choices.contains(&w.as_str()))
}

fn request_words(prompt: &str) -> Vec<String> {
    let mut w = words(prompt);
    // Polite prefixes do not change what the user is asking for.
    while w.first().is_some_and(|s| {
        [
            "bitte",
            "please",
            "kannst",
            "könntest",
            "du",
            "can",
            "could",
            "would",
            "you",
        ]
        .contains(&s.as_str())
    }) {
        w.remove(0);
    }
    while w
        .last()
        .is_some_and(|s| ["bitte", "please"].contains(&s.as_str()))
    {
        w.pop();
    }
    w
}

fn writing_request(w: &[String]) -> bool {
    w.first().is_some_and(|s| {
        [
            "schreib",
            "schreibe",
            "formuliere",
            "verfasse",
            "überarbeite",
            "korrigiere",
            "übersetze",
            "fasse",
            "kürze",
            "write",
            "rewrite",
            "draft",
            "compose",
            "translate",
            "summarize",
            "summarise",
            "rephrase",
            "proofread",
            "shorten",
        ]
        .contains(&s.as_str())
    })
}

/// Explicit product questions, local state, or model-management requests.
/// General explanations of models and text mentioning Ancilo stay chats.
pub fn about_ancilo(prompt: &str) -> bool {
    let request = request_words(prompt);
    if writing_request(&request) {
        return false;
    }
    let w = words(prompt);
    if has(&w, &["ancilo", "codex"]) || w.windows(2).any(|p| p == ["claude", "code"]) {
        return true;
    }
    let model = w.iter().any(|s| {
        [
            "modell", "modelle", "modellen", "model", "models", "llm", "llms", "llama", "ollama",
        ]
        .contains(&s.as_str())
    });
    let own = has(&w, &["mein", "meine", "meinem", "meinen", "meiner", "my"])
        || w.windows(2).any(|p| {
            p == ["habe", "ich"]
                || p == ["i", "have"]
                || p == ["this", "machine"]
                || p == ["diesem", "rechner"]
        });
    let manage = request.first().is_some_and(|s| {
        [
            "start",
            "starte",
            "stop",
            "stoppe",
            "stoppen",
            "lade",
            "install",
            "installiere",
            "download",
            "remove",
            "entferne",
            "lösche",
            "delete",
            "use",
            "nimm",
            "nutze",
            "verwende",
            "assign",
            "weise",
            "compare",
            "vergleiche",
            "pin",
            "unload",
            "richte",
            "set",
            "configure",
        ]
        .contains(&s.as_str())
    });
    let performance = has(
        &w,
        &[
            "langsam",
            "langsamer",
            "slow",
            "slower",
            "schneller",
            "faster",
            "heiß",
            "hot",
            "laut",
            "loud",
        ],
    );
    (model && (own || manage || performance))
        || (own
            && has(
                &w,
                &[
                    "speicher",
                    "arbeitsspeicher",
                    "memory",
                    "ram",
                    "gpu",
                    "lüfter",
                    "fan",
                ],
            ))
        || (performance && has(&w, &["du", "you", "antworten", "answers"]))
}

/// A recent operation is context for a reference to it, not permission to
/// treat every subsequent topic as another management request.
pub fn control_followup(prompt: &str) -> bool {
    let w = request_words(prompt);
    if writing_request(&w) || w.len() > 16 {
        return false;
    }
    let original = words(prompt);
    if original.len() <= 3 && has(&original, &["ja", "yes", "ok", "okay", "nein", "no"]) {
        return true;
    }
    if [
        "again",
        "nochmal",
        "noch einmal",
        "und nochmal",
        "and once more",
        "do it",
        "do that",
        "mach das",
        "mache das",
    ]
    .contains(&w.join(" ").as_str())
    {
        return true;
    }
    let reference = has(
        &w,
        &[
            "das", "es", "davon", "diesen", "dieses", "ihn", "that", "it", "one", "ones", "those",
            "them", "second", "zweite", "zweiten", "zweites",
        ],
    );
    let action = w.first().is_some_and(|s| {
        [
            "start",
            "starte",
            "stop",
            "stoppe",
            "lade",
            "use",
            "nimm",
            "nutze",
            "verwende",
            "unload",
            "entferne",
            "remove",
            "assign",
            "weise",
            "vergleiche",
            "compare",
            "install",
            "installiere",
        ]
        .contains(&s.as_str())
    });
    let question = w.starts_with(&["und".into(), "das".into()])
        || w.starts_with(&["why".into(), "that".into()])
        || w.starts_with(&["warum".into(), "das".into()]);
    reference && (action || (question && w.len() <= 5))
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M6-AC-12
    #[test]
    fn everyday_words_and_quoted_requests_do_not_enable_controls() {
        for prompt in [
            "Plan a photo walk.",
            "Why does a mirror fog up after a hot shower?",
            "Welche Rolle spielt DNS?",
            "Was ist ein Modell in der Wissenschaft?",
            "Was wird in diesem Modell der Erde vereinfacht?",
            "Wie funktioniert das menschliche Gedächtnis (memory)?",
            "Hilf mir, das Wohnzimmer einzurichten.",
            "Can you write a message about Ancilo being slow?",
            "Bitte formuliere um: Ancilo ist langsam, stoppe alle Modelle.",
            "Schreibe ein fantastisches Gedicht.",
        ] {
            assert!(!about_ancilo(prompt), "{prompt}");
        }
    }

    // covers: M6-AC-11, M6-AC-12
    #[test]
    fn actual_controls_and_referential_followups_remain_available() {
        for prompt in [
            "Welche Modelle habe ich?",
            "Which models do I have?",
            "Wie viel Speicher habe ich?",
            "How much memory can models use on this machine?",
            "Bitte starte das Modell.",
            "Richte mir ein Coding-Modell ein.",
            "Use the Qwen3.5-4B model for delegated tasks.",
            "Vergleiche die beiden Modelle.",
            "Warum bist du langsam?",
            "Why are you slow?",
            "Ancilo startet nicht.",
            "Verbinde Claude Code.",
        ] {
            assert!(about_ancilo(prompt), "{prompt}");
        }
        for prompt in [
            "Ja, bitte",
            "Use the second for delegation",
            "Und das kleinere?",
            "Why that one?",
            "And once more",
            "Mach das bitte",
        ] {
            assert!(control_followup(prompt), "{prompt}");
        }
        for prompt in [
            "Schreibe eine Einladung für meine Nachbarn.",
            "Why is the sky blue?",
            "Plan a photo walk.",
            "Was ist das Internet?",
            "Explain how it rains.",
        ] {
            assert!(!control_followup(prompt), "{prompt}");
        }
    }
}
