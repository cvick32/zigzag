//! Full-corpus sizing regression runner. See tests/PES_REGRESSION.md.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use zigzag::{
    document::Document,
    formats::{pes, read::read_stitch_file},
    geom::{P, scale_about},
};

fn discover(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            discover(&p, out)?;
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pes")) {
            out.push(p);
        }
    }
    Ok(())
}
fn engine_hash(dir: &Path, bytes: &mut Vec<u8>) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    for p in entries {
        if p.is_dir() {
            engine_hash(&p, bytes)?;
        } else if p.extension().is_some_and(|e| e == "rs") {
            bytes.extend(std::fs::read(p)?);
        }
    }
    Ok(())
}
fn fingerprint(bytes: &[u8]) -> String {
    let h = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    format!("{h:016x}")
}
fn signature(d: &Document) -> Result<Value> {
    let plan = d.plan();
    if plan.stitch_count() == 0 {
        bail!("empty stitch plan");
    }
    if plan
        .blocks
        .iter()
        .flat_map(|b| &b.runs)
        .flat_map(|r| &r.pts)
        .any(|p| !p.x.is_finite() || !p.y.is_finite())
    {
        bail!("non-finite stitch coordinate");
    }
    let bytes = pes::write_pes(&plan);
    let decoded = read_stitch_file(&bytes, "pes")?;
    if decoded.blocks.is_empty() {
        bail!("export reimport is empty");
    }
    let b = plan.bounds();
    Ok(
        json!({"stitches":plan.stitch_count(), "colors":plan.blocks.iter().map(|b| b.color).collect::<Vec<_>>(), "width_units":(b.width()*100.0).round() as i64, "height_units":(b.height()*100.0).round() as i64, "pes_hash":fingerprint(&bytes)}),
    )
}
fn check(path: &Path) -> Result<Value> {
    let input = std::fs::read(path)?;
    let d = Document::open(path)?;
    let original = signature(&d)?;
    let mut cases = Vec::new();
    for factor in [0.5, 2.0] {
        let case = (|| -> Result<Value> {
            let mut resized = d.clone();
            let ids: Vec<_> = (0..d.shapes.len()).collect();
            let report =
                resized.transform_keep_density(&ids, scale_about(P::default(), factor, factor))?;
            let result = signature(&resized)?;
            let a = d.plan().bounds();
            let b = resized.plan().bounds();
            for (old, new) in [(a.width(), b.width()), (a.height(), b.height())] {
                if (new - old * factor).abs() > 5.0 + old * factor * 0.05 {
                    bail!("{factor}x bounds differ from requested scale: {old} -> {new} units");
                }
            }
            let svg = resized.to_svg()?;
            let reopened = Document::from_svg(&svg, &resized.name, None)?;
            let left = resized.plan();
            let right = reopened.plan();
            if left.blocks.len() != right.blocks.len() {
                bail!("save/reopen changed color blocks");
            }
            for (a, b) in left.blocks.iter().zip(&right.blocks) {
                if a.color != b.color || a.runs.len() != b.runs.len() {
                    bail!("save/reopen changed thread or run order");
                }
                for (a, b) in a.runs.iter().zip(&b.runs) {
                    if a.trim_before != b.trim_before || a.pts.len() != b.pts.len() {
                        bail!("save/reopen changed stitches or trims");
                    }
                    if a.pts
                        .iter()
                        .zip(&b.pts)
                        .any(|(a, b)| (*a + resized.offset).dist(*b + reopened.offset) > 1.0)
                    {
                        bail!("save/reopen moved stitches beyond PES 0.1 mm resolution");
                    }
                }
            }
            // Independent runs must produce exactly the same exported needle positions.
            let mut repeat = d.clone();
            repeat.transform_keep_density(&ids, scale_about(P::default(), factor, factor))?;
            if signature(&repeat)? != result {
                bail!("{factor}x nondeterministic resize");
            }
            Ok(
                json!({"scale":factor,"fills":report.fills,"satins":report.satins,"repeated_runs":report.repeated_runs,"warnings":report.warnings.iter().map(|w| (&w.label,&w.reason)).collect::<Vec<_>>(),"result":result}),
            )
        })();
        cases.push(match case {
            Ok(v) => v,
            Err(e) => json!({"scale":factor,"error":format!("{e:#}")}),
        });
    }
    Ok(json!({"source_hash":fingerprint(&input),"original":original,"cases":cases}))
}
fn failed(v: &Value) -> bool {
    v.get("error").is_some()
        || v["data"]["cases"]
            .as_array()
            .is_some_and(|cases| cases.iter().any(|c| c.get("error").is_some()))
}
fn read_rows(path: &Path) -> Result<BTreeMap<String, Value>> {
    let mut rows = BTreeMap::new();
    if path.exists() {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            let v: Value = serde_json::from_str(&line?)?;
            let key = v["file"].as_str().context("missing file key")?.to_owned();
            if rows.insert(key.clone(), v).is_some() {
                bail!("duplicate entry: {key}");
            }
        }
    }
    Ok(rows)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let option = |flag: &str, default: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or(default.into())
    };
    if args.iter().any(|a| a == "--help") {
        println!(
            "pes_regression [--root examples/pes] [--report target/pes-sizing.jsonl] [--baseline tests/pes-sizing-baseline.jsonl] [--jobs 4] [--filter substring] [--resume] [--bless]\nDefault compares against baseline; --bless explicitly creates/replaces it after a successful full run. --resume uses existing report entries only when their source fingerprint still matches."
        );
        return Ok(());
    }
    let root = PathBuf::from(option("--root", "examples/pes"));
    let report_path = PathBuf::from(option("--report", "target/pes-sizing.jsonl"));
    let baseline_path = PathBuf::from(option("--baseline", "tests/pes-sizing-baseline.jsonl"));
    let filter = option("--filter", "");
    let bless = args.iter().any(|a| a == "--bless");
    if bless && !filter.is_empty() {
        bail!("cannot bless a filtered corpus");
    }
    let mut files = Vec::new();
    discover(&root, &mut files)?;
    files.sort();
    files.retain(|p| {
        p.strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .contains(&filter)
    });
    if files.is_empty() {
        bail!("no PES fixtures found");
    }
    let baseline = read_rows(&baseline_path)?;
    let mut engine = Vec::new();
    engine_hash(Path::new("src"), &mut engine)?;
    engine.extend(std::fs::read("examples/pes_regression.rs")?);
    engine.extend(std::fs::read("Cargo.lock")?);
    let engine = fingerprint(&engine);
    let stamp = report_path.with_extension("engine");
    if args.iter().any(|a| a == "--resume")
        && report_path.exists()
        && std::fs::read_to_string(&stamp).unwrap_or_default() != engine
    {
        bail!("engine changed since checkpoint; use a fresh report instead of --resume");
    }
    if let Some(p) = report_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(stamp, engine)?;
    let mut rows = if args.iter().any(|a| a == "--resume") {
        read_rows(&report_path)?
    } else {
        BTreeMap::new()
    };
    rows.retain(|name, row| {
        !failed(row)
            && files.iter().any(|p| {
                p.strip_prefix(&root).unwrap().to_string_lossy() == name.as_str()
                    && std::fs::read(p).is_ok_and(|b| row["data"]["source_hash"] == fingerprint(&b))
            })
    });
    if let Some(p) = report_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut output = std::fs::File::create(&report_path)?;
    for row in rows.values() {
        writeln!(output, "{row}")?;
    }
    let pending: Vec<_> = files
        .iter()
        .filter(|p| !rows.contains_key(p.strip_prefix(&root).unwrap().to_str().unwrap()))
        .cloned()
        .collect();
    let output = Arc::new(Mutex::new(output));
    let rows = Arc::new(Mutex::new(rows));
    let cursor = AtomicUsize::new(0);
    let jobs: usize = option("--jobs", "4").parse()?;
    if jobs == 0 {
        bail!("jobs must be positive");
    }
    eprintln!(
        "{} fixtures, {} pending, {jobs} workers",
        files.len(),
        pending.len()
    );
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let output = &output;
            let rows = &rows;
            let pending = &pending;
            let root = &root;
            let cursor = &cursor;
            scope.spawn(move || {
                loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = pending.get(i) else { break };
                    let name = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned();
                    let result = std::panic::catch_unwind(|| check(path));
                    let row = match result {
                        Ok(Ok(data)) => json!({"file":name,"data":data}),
                        Ok(Err(e)) => json!({"file":name,"error":format!("{e:#}")}),
                        Err(_) => json!({"file":name,"error":"panic during resize"}),
                    };
                    let mut file = output.lock().unwrap();
                    writeln!(file, "{row}").unwrap();
                    file.flush().unwrap();
                    drop(file);
                    rows.lock().unwrap().insert(name, row);
                    if (i + 1).is_multiple_of(100) {
                        eprintln!("processed {} / {}", i + 1, pending.len());
                    }
                }
            });
        }
    });
    let rows = rows.lock().unwrap();
    let failures: Vec<_> = rows
        .iter()
        .filter(|(_, v)| failed(v))
        .map(|(k, _)| k)
        .collect();
    let changed: Vec<_> = rows
        .iter()
        .filter(|(k, v)| baseline.get(*k) != Some(*v))
        .map(|(k, _)| k)
        .collect();
    let removed: Vec<_> = baseline
        .keys()
        .filter(|k| k.contains(&filter) && !rows.contains_key(*k))
        .collect();
    let summary = json!({"fixtures":files.len(),"failures":failures,"changed_or_new":changed,"removed":removed});
    std::fs::write(
        report_path.with_extension("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    eprintln!(
        "{} failures, {} changed/new, {} removed",
        failures.len(),
        changed.len(),
        removed.len()
    );
    if !failures.is_empty() {
        bail!("corpus failures: see report; baseline not updated");
    }
    if bless {
        if let Some(p) = baseline_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let temp = baseline_path.with_extension("tmp");
        let mut out = std::fs::File::create(&temp)?;
        for row in rows.values() {
            writeln!(out, "{row}")?;
        }
        out.sync_all()?;
        std::fs::rename(temp, baseline_path)?;
    } else if !changed.is_empty() || !removed.is_empty() {
        bail!("baseline differences: inspect report before explicitly using --bless");
    }
    Ok(())
}
