//! `rtk toon` — manage TOON mirrors of JSON files.
//!
//! JSON stays canonical; the `.toon` mirror is a regenerable working copy an
//! agent reads and edits at roughly a third of the size.

use crate::core::lens::mirror;
use crate::ToonAction;
use anyhow::{bail, Result};
use std::path::Path;

pub fn run(action: ToonAction) -> Result<()> {
    match action {
        ToonAction::Extract { files } => each(&files, extract_one),
        ToonAction::Compile { files } => each(&files, compile_one),
        ToonAction::Check { files } => check(&files),
    }
}

fn each(files: &[std::path::PathBuf], op: fn(&Path) -> Result<()>) -> Result<()> {
    let mut failed = 0;
    for file in files {
        if let Err(e) = op(file) {
            eprintln!("rtk toon: {e:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn extract_one(path: &Path) -> Result<()> {
    let w = mirror::extract(path)?;
    let saved = 100 - (w.to_bytes * 100 / w.from_bytes.max(1));
    println!(
        "{} -> {} ({} -> {} bytes, -{}%)",
        path.display(),
        w.path.display(),
        w.from_bytes,
        w.to_bytes,
        saved
    );
    if !w.byte_identical {
        println!(
            "  note: regenerating {} normalizes number/escape forms (e.g. 64.0 -> 64); values are unchanged",
            path.display()
        );
    }
    Ok(())
}

fn compile_one(path: &Path) -> Result<()> {
    let w = mirror::compile(path)?;
    println!("{} -> {} ({} bytes)", path.display(), w.path.display(), w.to_bytes);
    Ok(())
}

fn check(files: &[std::path::PathBuf]) -> Result<()> {
    let mut drifted = Vec::new();
    for file in files {
        match mirror::in_sync(file) {
            Ok(true) => {}
            Ok(false) => drifted.push(format!("{}: mirror and source differ", file.display())),
            Err(e) => drifted.push(format!("{}: {e:#}", file.display())),
        }
    }
    if drifted.is_empty() {
        println!("{} mirror(s) in sync", files.len());
        return Ok(());
    }
    for d in &drifted {
        eprintln!("rtk toon check: {d}");
    }
    bail!("{} mirror(s) out of sync", drifted.len())
}
