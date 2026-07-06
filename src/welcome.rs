use crate::config;
use colored::Colorize;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;

/// The Wavedash glyph — a diamond chasing two chevrons — rendered with
/// half-block characters (▀▄█) so the diagonals stay crisp at this size.
/// Sampled from the source `glyph.svg`; 20 columns wide, 10 rows tall.
const GLYPH: &str = "        ▄██▄
         ▀███▄
    ▄█▄    ▀███▄
     ▀██▄    ▀███▄
▄█▄    ▀██▄    ▀███▄
▀█▀    ▄██▀    ▄███▀
     ▄██▀    ▄███▀
    ▀█▀    ▄███▀
         ▄███▀
        ▀██▀";

/// Width (in columns) the glyph is padded to so the wordmark lines up.
const GLYPH_WIDTH: usize = 20;

fn marker_path() -> Option<PathBuf> {
    config::wavedash_dir().ok().map(|d| d.join(".welcomed"))
}

/// Print the first-run splash once, the very first time the CLI is run on an
/// interactive terminal. A marker file under the wavedash config dir records
/// that we've greeted, so it never shows again. No-ops when stdout isn't a TTY
/// (piped output, CI) so it never pollutes machine-readable output.
pub fn show_first_run_if_needed() {
    if !std::io::stdout().is_terminal() {
        return;
    }
    let Some(marker) = marker_path() else {
        return;
    };
    if marker.exists() {
        return;
    }

    print_splash();
    mark_welcomed();
}

/// The home screen shown when `wavedash` is run with no subcommand. Always
/// prints the splash (this is an explicit "show me the overview" invocation)
/// and points at `--help` for the full command list. Also records the greeting
/// so a later first command doesn't greet again.
pub fn show_home() {
    print_splash();
    println!(
        "  Run {} to see every command.",
        "wavedash --help".bold().bright_white()
    );
    println!();
    mark_welcomed();
}

/// Record that we've greeted so the first-run banner never repeats. Failures
/// here must never block the CLI, so errors are intentionally ignored.
fn mark_welcomed() {
    let Some(marker) = marker_path() else {
        return;
    };
    if let Some(parent) = marker.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&marker, b"1");
}

fn print_splash() {
    let lines: Vec<&str> = GLYPH.lines().collect();
    let wordmark = "W A V E D A S H".bold().bright_white();
    let tagline = "Play anywhere instantly".dimmed();

    println!();
    for (i, line) in lines.iter().enumerate() {
        // Pad the plain text first, then colorize — coloring before padding
        // would pad the ANSI escape codes and break alignment.
        let glyph = format!("{:<width$}", line, width = GLYPH_WIDTH)
            .bright_white()
            .bold();
        match i {
            4 => println!("  {}    {}", glyph, wordmark),
            5 => println!("  {}    {}", glyph, tagline),
            _ => println!("  {}", glyph),
        }
    }

    println!();
    println!("  {}", "Get started".bold().bright_white());
    let steps = [
        ("wavedash auth login", "Sign in to your account"),
        ("wavedash init", "Set up this project"),
        ("wavedash dev", "Preview your game locally"),
        ("wavedash build push", "Upload a build"),
    ];
    for (cmd, desc) in steps {
        let cmd = format!("{:<22}", cmd).bold();
        println!("    {} {} {}", "$".dimmed(), cmd, desc.dimmed());
    }
    println!();
}
