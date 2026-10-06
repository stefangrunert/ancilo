//! Test data for trying the Feature Port Lab features by hand:
//! `cargo run -p ancilo-docs --example testdata -- <dir>` writes
//! `<dir>/Belege` (documents to ask about), `<dir>/Rechnungen` (a folder
//! for a task) and `<dir>/Lange-Aufgabe` (a project for a long task).
use ancilo_docs::write::{Sheet, docx, xlsx};
use std::path::Path;

fn w(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let d = Path::new(&dir);
    // 1. Documents (sources in the chat).
    let filler = |n: usize| -> String {
        (0..n)
            .map(|i| format!("§ {} Allgemeines {i}\n\nDie Parteien regeln Einzelheiten zu Punkt {i} in gegenseitigem Einvernehmen. Änderungen bedürfen der Textform.", i + 20))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let lease = format!(
        "# § 1 Mietsache\n\nVermietet wird die Wohnung im 2. Obergeschoss, Lindenstraße 12, 80331 München.\n\n# § 2 Miete\n\nDie Kaltmiete beträgt 1.150,00 EUR monatlich, die Nebenkostenvorauszahlung 230,00 EUR.\n\n# § 9 Kündigung\n\nDas Mietverhältnis kann von beiden Seiten mit einer Frist von drei Monaten zum Monatsende schriftlich gekündigt werden.\n\n# § 10 Kaution\n\nDie Kaution beträgt drei Kaltmieten und ist in drei Raten zahlbar.\n\n{}",
        filler(40)
    );
    w(
        &d.join("Belege/Verträge/Mietvertrag.docx"),
        &docx(Some("Mietvertrag"), &lease).unwrap(),
    );
    w(
        &d.join("Belege/Hausordnung.md"),
        format!("# Hausordnung\n\nRuhezeiten gelten von 22 Uhr bis 6 Uhr sowie sonntags ganztägig.\n\nFahrräder gehören in den Fahrradkeller, nicht in den Hausflur.\n\n{}", filler(30)).as_bytes(),
    );
    let mut rows = vec![vec![
        "Monat".into(),
        "Heizung".into(),
        "Wasser".into(),
        "Strom Allgemein".into(),
    ]];
    for (m, h, wa, s) in [
        ("Januar", "120,40", "31,20", "12,10"),
        ("Februar", "110,80", "29,90", "11,95"),
        ("März", "95,10", "30,05", "12,30"),
    ] {
        rows.push(vec![m.into(), h.into(), wa.into(), s.into()]);
    }
    let mut rows24 = rows.clone();
    rows24[2][1] = "140,00".into();
    w(
        &d.join("Belege/Nebenkosten.xlsx"),
        &xlsx(&[
            Sheet {
                name: "2024".into(),
                rows: rows24,
            },
            Sheet {
                name: "2025".into(),
                rows,
            },
        ])
        .unwrap(),
    );
    // 2. A folder for a task (results checked before keeping).
    for (name, firm, date, amount) in [
        (
            "rechnung-stadtwerke.txt",
            "Stadtwerke München",
            "03.01.2026",
            "84,20 EUR",
        ),
        ("rechnung-telekom.txt", "Telekom", "09.01.2026", "39,99 EUR"),
        (
            "rechnung-hausverwaltung.txt",
            "Hausverwaltung Lind",
            "15.01.2026",
            "1.380,00 EUR",
        ),
    ] {
        w(
            &d.join("Rechnungen").join(name),
            format!("Rechnung\nFirma: {firm}\nDatum: {date}\nBetrag: {amount}\n").as_bytes(),
        );
    }
    // 3. A project for a long task: a one-off import whose findings are in
    //    the middle of a long output (and gone afterwards: the batch is archived).
    let mut csv = String::from("customer,amount\n");
    for i in 1..=2400 {
        let (c, a) = match i {
            917 => ("K-2291".to_string(), "\"12,50\"".to_string()),
            1873 => ("K-4471".to_string(), "n/a".to_string()),
            _ => (
                format!("K-{}", 1000 + (i * 37) % 9000),
                format!("{}.50", (i * 7) % 300),
            ),
        };
        csv.push_str(&format!("{c},{a}\n"));
    }
    w(&d.join("Lange-Aufgabe/incoming/orders.csv"), csv.as_bytes());
    w(
        &d.join("Lange-Aufgabe/import.py"),
        br#"import csv, os, shutil, sys
src = "incoming/orders.csv"
if not os.path.exists(src):
    print("nothing to import: incoming/ is empty (the last batch was archived)")
    sys.exit(0)
rows = list(csv.reader(open(src)))[1:]
bad = 0
for i, (customer, amount) in enumerate(rows, start=1):
    try:
        float(amount)
        print(f"row {i:04d} ok   customer {customer} amount {amount}")
    except ValueError:
        bad += 1
        print(f"row {i:04d} REJECTED: amount {amount!r} is not a number (customer {customer})")
print(f"imported {len(rows) - bad} of {len(rows)} rows")
os.makedirs("archive", exist_ok=True)
shutil.move(src, "archive/orders-batch-17.csv")
"#,
    );
    println!("test data in {}", d.display());
}
