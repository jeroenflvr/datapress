//! Startup banner — printed once when a backend boots.
//!
//! Lives in `datapress-core` so both backend crates (and the Python
//! binding's embedded server) render the exact same splash.

/// DataPress version (matches the workspace `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const ASCII_ART: &str = r#"
██████╗  █████╗ ████████╗ █████╗ ██████╗       ██████╗ ███████╗
██╔══██╗██╔══██╗╚══██╔══╝██╔══██╗██╔══██╗      ██╔══██╗██╔════╝
██║  ██║███████║   ██║   ███████║██████╔╝█████╗██████╔╝███████╗
██║  ██║██╔══██║   ██║   ██╔══██║██╔═══╝ ╚════╝██╔══██╗╚════██║
██████╔╝██║  ██║   ██║   ██║  ██║██║           ██║  ██║███████║
╚═════╝ ╚═╝  ╚═╝   ╚═╝   ╚═╝  ╚═╝╚═╝           ╚═╝  ╚═╝╚══════╝
"#;

/// Brand yellow (`--dp-yellow` #f7a41d) as a 24-bit ANSI color escape.
const DP_YELLOW: &str = "\x1b[38;2;247;164;29m";
const RESET: &str = "\x1b[0m";

/// Write the banner + version straight to stdout. Bypasses `log` so it
/// always shows regardless of `RUST_LOG`.
pub fn print() {
    println!("{DP_YELLOW}{ASCII_ART}{RESET}");
    println!("{DP_YELLOW}  datap-rs v{VERSION}{RESET}");
    println!("{DP_YELLOW}  doc: https://docs.datap.rs{RESET}");
    println!();
}
