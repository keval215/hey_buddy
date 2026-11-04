//! `bench` CLI: see README "Latency benchmark".

use bench::{build_parts, render_table, run, Config};
use std::path::PathBuf;

const USAGE: &str =
    "usage: bench [--out PATH] [--models-dir DIR] [--stt-model ID] [--profile NAME] [--force-fake]
             [--stt-delay-ms N] [--llm-first-token-delay-ms N] [--llm-delta-delay-ms N]
             [--tts-delay-ms N]
--profile reads models/registry.json (default: `default`, the rtx2050-4gb profile).
Delays apply to fake stages only. HEY_BUDDY_MODELS_DIR is the default for --models-dir.";

fn parse(args: &[String]) -> Result<(Config, PathBuf), String> {
    let mut cfg = Config::default();
    let mut out = PathBuf::from("bench-results.json");
    cfg.models_dir = std::env::var_os("HEY_BUDDY_MODELS_DIR").map(PathBuf::from);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        let num = |s: String, name: &str| {
            s.parse::<u64>()
                .map_err(|_| format!("{name} needs a whole number"))
        };
        match a.as_str() {
            "--out" => out = PathBuf::from(val("--out")?),
            "--models-dir" => cfg.models_dir = Some(PathBuf::from(val("--models-dir")?)),
            "--stt-model" => cfg.stt_model = Some(val("--stt-model")?),
            "--profile" => cfg.profile = val("--profile")?,
            "--force-fake" => cfg.force_fake = true,
            "--stt-delay-ms" => cfg.stt_delay_ms = num(val(a)?, a)?,
            "--llm-first-token-delay-ms" => cfg.llm_first_token_delay_ms = num(val(a)?, a)?,
            "--llm-delta-delay-ms" => cfg.llm_delta_delay_ms = num(val(a)?, a)?,
            "--tts-delay-ms" => cfg.tts_delay_ms = num(val(a)?, a)?,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok((cfg, out))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let (cfg, out) = match parse(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let result = build_parts(&cfg).and_then(|(parts, notes)| {
        for n in &notes {
            println!("{n}");
        }
        run(&cfg, parts)
    });
    match result {
        Ok(report) => {
            println!("{}", render_table(&report));
            let json = serde_json::to_string_pretty(&report).expect("report serialises");
            if let Err(e) = std::fs::write(&out, json + "\n") {
                eprintln!("error: cannot write {}: {e}", out.display());
                std::process::exit(1);
            }
            println!("wrote {}", out.display());
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
