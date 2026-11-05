//! `registry-check [path]` (default `models/registry.json`). Exit 1 and list problems on failure.

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/registry.json".to_string());
    let json = match std::fs::read_to_string(&path) {
        Ok(j) => j,
        Err(_) => {
            eprintln!("registry-check: cannot read the registry file");
            std::process::exit(1);
        }
    };
    match registry_check::check(&json) {
        Ok(s) => println!(
            "registry-check: OK ({} entries, {} profiles, {} with a download source)",
            s.entries, s.profiles, s.with_source
        ),
        Err(errs) => {
            for e in errs {
                eprintln!("registry-check: {e}");
            }
            std::process::exit(1);
        }
    }
}
