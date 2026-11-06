//! `registry-verify --id <entry> --url <https url> [--max-bytes N] [--registry <file>]`
//!
//! Explicit maintainer run (S3-5). See `docs/models.md`. Never edits the registry.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = registry_verify::main_with(
        &args,
        &buddy_core::download::Downloader::new(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    std::process::exit(code);
}
