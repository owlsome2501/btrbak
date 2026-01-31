use console::{Style, Term};
use std::io::Write as _;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

struct UiState {
    verbosity: Verbosity,
    term: Term,
}

static UI: OnceLock<UiState> = OnceLock::new();

pub fn init(verbose: bool, quiet: bool) {
    let verbosity = if quiet {
        Verbosity::Quiet
    } else if verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };

    let _ = UI.set(UiState {
        verbosity,
        term: Term::stderr(),
    });
}

fn state() -> &'static UiState {
    UI.get_or_init(|| UiState {
        verbosity: Verbosity::Normal,
        term: Term::stderr(),
    })
}

/// `\n== {msg} ==\n` (bold cyan)
pub fn header(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().bold().cyan();
    let _ = s.term.write_line(&format!("\n{}\n", style.apply_to(format!("== {} ==", msg))));
}

/// `[{cur}/{total}] {msg}` (bold)
pub fn step(cur: usize, total: usize, msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().bold();
    let _ = s.term.write_line(&format!(
        "{}",
        style.apply_to(format!("[{}/{}] {}", cur, total, msg))
    ));
}

/// `  -> {msg}` (dim)
pub fn substep(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().dim();
    let _ = s.term.write_line(&format!("  {} {}", style.apply_to("->"), msg));
}

/// `  ✓ {msg}` (green)
pub fn success(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().green();
    let _ = s.term.write_line(&format!("  {}", style.apply_to(format!("\u{2713} {}", msg))));
}

/// `  {msg}`
pub fn info(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let _ = s.term.write_line(&format!("  {}", msg));
}

/// `     {msg}` (dim) - only in verbose mode
pub fn detail(msg: &str) {
    let s = state();
    if s.verbosity != Verbosity::Verbose {
        return;
    }
    let style = Style::new().dim();
    let _ = s.term.write_line(&format!("     {}", style.apply_to(msg)));
}

/// `  ⚠ {msg}` (yellow) - always shown
pub fn warning(msg: &str) {
    let s = state();
    let style = Style::new().yellow();
    let _ = s.term.write_line(&format!("  {}", style.apply_to(format!("\u{26a0} {}", msg))));
}

/// `  ✗ {msg}` (red) - always shown
pub fn error(msg: &str) {
    let s = state();
    let style = Style::new().red();
    let _ = s.term.write_line(&format!("  {}", style.apply_to(format!("\u{2717} {}", msg))));
}

/// Error with indented hint lines - always shown
pub fn error_with_hints(msg: &str, hints: &[&str]) {
    error(msg);
    if !hints.is_empty() {
        let s = state();
        let _ = s.term.write_line("");
        let style = Style::new().dim();
        let _ = s.term.write_line(&format!("  {}", style.apply_to("Hints:")));
        for hint in hints {
            let _ = s.term.write_line(&format!("    {}", style.apply_to(format!("\u{2022} {}", hint))));
        }
    }
}

/// `  $ {cmd_str}` (dim cyan)
pub fn cmd_start(cmd_str: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().dim().cyan();
    let _ = s.term.write_line(&format!("  {}", style.apply_to(format!("$ {}", cmd_str))));
}

/// `    stderr: {line}` (yellow) for each line
pub fn cmd_stderr_output(text: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().yellow();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let _ = s.term.write_line(&format!("    {}", style.apply_to(format!("stderr: {}", trimmed))));
        }
    }
}

/// Print a blank line as section separator
pub fn section_end() {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let _ = s.term.write_line("");
}

/// Format byte count into human-readable string (e.g. "12.34 MiB")
pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{} B", bytes)
    }
}

/// In-place progress update on the current line (no trailing newline).
/// Overwrites previous content via `\r`.
pub fn transfer_progress(transferred: u64, speed: u64) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().dim().cyan();
    let msg = format!(
        "    {} | {}/s",
        format_bytes(transferred),
        format_bytes(speed),
    );
    // \r moves cursor to start of line; clear_line removes old text
    let _ = s.term.clear_line();
    let _ = write!(&s.term, "\r{}", style.apply_to(&msg));
    let _ = s.term.flush();
}

/// Finalize the progress line with final stats, replacing the live line.
pub fn transfer_done(transferred: u64, elapsed_secs: f64) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let avg_speed = if elapsed_secs > 0.0 {
        (transferred as f64 / elapsed_secs) as u64
    } else {
        0
    };
    let style = Style::new().dim().cyan();
    let msg = format!(
        "    {} transferred in {:.1}s ({}/s)",
        format_bytes(transferred),
        elapsed_secs,
        format_bytes(avg_speed),
    );
    let _ = s.term.clear_line();
    let _ = s.term.write_line(&format!("\r{}", style.apply_to(&msg)));
}

/// Format a `Command` into a display string
pub fn format_cmd(cmd: &Command) -> String {
    let program = cmd.get_program().to_string_lossy();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.contains(' ') {
                format!("'{}'", s)
            } else {
                s.to_string()
            }
        })
        .collect();

    if args.is_empty() {
        program.to_string()
    } else {
        format!("{} {}", program, args.join(" "))
    }
}
