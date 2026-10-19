//! Assign explicit file instructions before extracting columns, sections or
//! totals. File names are anchors, not requirements themselves. This is a
//! deterministic reading of named instructions, not a language-model judge.

use std::collections::BTreeMap;

use regex::Regex;

/// Select the instructions for one result. Unnamed tasks retain the existing
/// checks. A named instruction also owns following unnamed clauses; explicit
/// "both/all files" instructions reset that scope. Later corrections keep
/// their order, and cannot cancel requirements belonging to another file.
pub(super) fn for_file(task: &str, path: &str, results: &[&str]) -> String {
    let mut names: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for &file in results {
        for name in [file, file.rsplit('/').next().unwrap_or(file)] {
            let owners = names.entry(name.to_lowercase()).or_default();
            if !owners.contains(&file) {
                owners.push(file);
            }
        }
    }
    if names.is_empty() {
        return task.to_string();
    }
    // Prefer a full path over a basename, and a longer name over its prefix.
    let mut aliases: Vec<_> = names.keys().collect();
    aliases.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let pattern = format!(
        r#"(?i)[`\"„“']?({})[`\"“”']?"#,
        aliases
            .iter()
            .map(|s| regex::escape(s))
            .collect::<Vec<_>>()
            .join("|")
    );
    // A very large result set may exceed the regex compilation budget.
    // Keep the existing checks rather than fail the preview request.
    let Ok(re) = Regex::new(&pattern) else {
        return task.to_string();
    };
    let filename_char = |c: char| c.is_alphanumeric() || "_./\\-".contains(c);
    let mentions: Vec<_> = re
        .captures_iter(task)
        .filter_map(|c| {
            let name = c.get(1).unwrap();
            if task[..name.start()]
                .chars()
                .next_back()
                .is_some_and(filename_char)
                || task[name.end()..].chars().next().is_some_and(|ch| {
                    filename_char(ch)
                        && !(ch == '.'
                            && task[name.end() + 1..]
                                .chars()
                                .next()
                                .is_none_or(char::is_whitespace))
                })
            {
                return None;
            }
            let whole = c.get(0).unwrap();
            Some((
                whole.start(),
                whole.end(),
                names.get(&name.as_str().to_lowercase())?,
            ))
        })
        .collect();
    if mentions.is_empty() {
        return task.to_string();
    }
    // Sentence punctuation never splits a filename (including quoted paths
    // with spaces). Decimal dots and abbreviations within words stay intact.
    let mut ends = Vec::new();
    for (at, ch) in task.char_indices() {
        let next = at + ch.len_utf8();
        if (";!?\n".contains(ch)
            || (ch == '.' && task[next..].chars().next().is_none_or(char::is_whitespace)))
            && !mentions.iter().any(|(a, b, _)| *a <= at && at < *b)
        {
            ends.push(next);
        }
    }
    ends.push(task.len());
    let mut start = 0;
    let mut selected = Vec::new();
    let mut active = results.to_vec();
    for end in ends {
        let refs: Vec<_> = mentions
            .iter()
            .filter(|(a, b, _)| *a >= start && *b <= end)
            .collect();
        // "a.csv and b.csv with columns X, Y" is shared. In contrast,
        // "a.csv with columns X, Y, and b.csv with sections Z" has two
        // instructions. Split at the last conjunction before the next name,
        // only once the preceding name already has its own description.
        let mut cuts = Vec::new();
        for pair in refs.windows(2) {
            let between = &task[pair[0].1..pair[1].0];
            let description = between
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .any(|w| !["and", "und", "sowie"].contains(&w.to_lowercase().as_str()));
            if description && let Some(m) = NEXT.find_iter(between).last() {
                cuts.push(pair[0].1 + m.start());
            }
        }
        cuts.push(end);
        for stop in cuts {
            let clause = &task[start..stop];
            let local: Vec<_> = refs
                .iter()
                .filter(|(a, b, _)| *a >= start && *b <= stop)
                .collect();
            if SHARED.is_match(clause) {
                active = results.to_vec();
            } else if !local.is_empty() {
                active = local
                    .iter()
                    .flat_map(|(_, _, owners)| owners.iter().copied())
                    .collect();
            }
            if active.contains(&path) {
                let mut text = clause.to_string();
                // Quoted file names must not become required content words.
                for &&(a, b, _) in local.iter().rev() {
                    text.replace_range(a - start..b - start, " ");
                }
                selected.push(text);
            }
            start = stop;
        }
    }
    selected.join("\n")
}

static NEXT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)(,|\b(and|und|sowie|then|danach)\b)\s*((and|und|then|danach|create|write|make|erstelle|schreibe|erzeuge|eine?|the|die|a|an)\s+)*$").expect("valid")
});
static SHARED: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)\b(both|all|each|every)\s+(files?|documents?|results?|outputs?)\b|\b(beide[nr]?|alle[nr]?|jede[nrs]?)\s+(dateien?|dokumente[n]?|ergebnisse[n]?)\b").expect("valid")
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{CheckLevel, check, layout};
    use crate::write::docx;

    fn scoped(task: &str, file: &str) -> String {
        for_file(task, file, &["Kosten.xlsx", "Bericht.docx"])
    }

    // covers: FPL-03
    #[test]
    fn named_results_do_not_inherit_each_others_requirements() {
        let task = "Erstelle zuerst Kosten.xlsx aus Belege.txt mit den Spalten Posten und Betrag und einer abschließenden Summe. Erstelle danach Bericht.docx mit einer ausführlichen Erklärung der einzelnen Kosten.";
        let word = layout(
            "Bericht.docx",
            &docx(None, "Strom: 120 Euro. Internet: 80 Euro.").unwrap(),
        )
        .unwrap();
        let findings = check(&word, &scoped(task, "Bericht.docx"));
        assert!(
            !findings.iter().any(|f| f.level == CheckLevel::Error),
            "{findings:?}"
        );
        let sheet = layout("Kosten.csv", b"Posten,Betrag\nStrom,120\nInternet,80").unwrap();
        let findings = check(&sheet, &scoped(task, "Kosten.xlsx"));
        assert!(
            findings
                .iter()
                .any(|f| f.level == CheckLevel::Error && f.message.contains("total")),
            "{findings:?}"
        );
    }

    // covers: FPL-03
    #[test]
    fn separate_instructions_in_one_sentence_and_same_format() {
        let task =
            "Create a.csv with columns Item and Amount, and b.csv with columns Date and Price.";
        let a = for_file(task, "a.csv", &["a.csv", "b.csv"]);
        let b = for_file(task, "b.csv", &["a.csv", "b.csv"]);
        assert!(a.contains("Item") && !a.contains("Date"), "{a}");
        assert!(b.contains("Date") && !b.contains("Item"), "{b}");
    }

    // covers: FPL-03
    #[test]
    fn shared_requirements_and_file_specific_corrections() {
        let task = "Erstelle a.csv und b.csv mit den Spalten Name und Datum. In a.csv: lass Datum weg. Beide Dateien brauchen die Spalte Betrag.";
        let a = super::super::required(&for_file(task, "a.csv", &["a.csv", "b.csv"]));
        let b = super::super::required(&for_file(task, "b.csv", &["a.csv", "b.csv"]));
        assert!(!a.iter().any(|x| x == "Datum"), "{a:?}");
        assert!(b.iter().any(|x| x == "Datum"), "{b:?}");
        for terms in [a, b] {
            assert!(terms.iter().any(|x| x == "Betrag"), "{terms:?}");
        }
    }

    // covers: FPL-03
    #[test]
    fn following_clauses_and_later_messages_keep_their_target() {
        let task = "Kosten.xlsx: Spalten Posten und Betrag. Füge eine Summe hinzu.\nBericht.docx: Abschnitte Ergebnis und Details.\nKosten.xlsx: keine Summe.\nBericht.docx: entferne Details.";
        let sheet = super::super::wanted(&scoped(task, "Kosten.xlsx"));
        assert!(!sheet.total);
        assert!(!sheet.terms.iter().any(|t| t.text == "Ergebnis"));
        let word = super::super::required(&scoped(task, "Bericht.docx"));
        assert!(word.iter().any(|t| t == "Ergebnis"));
        assert!(
            !word.iter().any(|t| t == "Posten" || t == "Details"),
            "{word:?}"
        );
    }

    // covers: FPL-03
    #[test]
    fn file_names_are_not_quoted_content_requirements() {
        let task = "Create `Kosten.xlsx` with columns Item and Amount. Create \"Bericht.docx\" with headings Summary and Details.";
        let required = super::super::required(&scoped(task, "Bericht.docx"));
        assert!(
            !required.iter().any(|t| t.contains("docx") || t == "Item"),
            "{required:?}"
        );
        assert!(required.iter().any(|t| t == "Summary"));
    }

    // covers: FPL-03
    #[test]
    fn exact_paths_spaces_and_unicode_do_not_match_other_names() {
        let files = ["Nord/Kosten 2026.xlsx", "Süd/Kosten 2026.xlsx"];
        let task = "In „Nord/Kosten 2026.xlsx“: Spalten Name und Betrag. In „Süd/Kosten 2026.xlsx“: Spalten Datum und Betrag.";
        let north = for_file(task, files[0], &files);
        assert!(
            north.contains("Name") && !north.contains("Datum"),
            "{north}"
        );
        let unrelated = "Use old-Kosten.xlsx with columns Old and Removed.";
        assert_eq!(scoped(unrelated, "Kosten.xlsx"), unrelated);
    }

    // covers: FPL-03
    #[test]
    fn unnamed_tasks_keep_all_existing_checks() {
        let task =
            "Erstelle eine Tabelle mit Spalten Name und Betrag und einer Summe.\nLass Name weg.";
        assert_eq!(scoped(task, "Kosten.xlsx"), task);
    }

    // covers: FPL-03
    #[test]
    fn stated_cost_totals_are_present_but_not_automatically_verified() {
        for text in [
            "Die Gesamtkosten betragen 200 Euro.",
            "3. Gesamtkosten\nBetrag: 200 Euro",
            "Total costs: 200 Euro.",
        ] {
            let word = layout("Bericht.docx", &docx(None, text).unwrap()).unwrap();
            let findings = check(&word, "Schreibe einen Bericht mit einer Gesamtsumme.");
            assert!(
                !findings.iter().any(|f| f.level == CheckLevel::Error),
                "{findings:?}"
            );
            assert!(
                !findings
                    .iter()
                    .any(|f| f.area == crate::preview::CheckArea::Numbers
                        && f.level == CheckLevel::Ok),
                "no arithmetic was checked: {findings:?}"
            );
        }
        for text in [
            "Die Gesamtkosten werden später ergänzt.",
            "Kosten: 200 Euro.",
        ] {
            let word = layout("Bericht.docx", &docx(None, text).unwrap()).unwrap();
            assert!(
                check(&word, "mit einer Gesamtsumme")
                    .iter()
                    .any(|f| f.level == CheckLevel::Error)
            );
        }
    }
}
