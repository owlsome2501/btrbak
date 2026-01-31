use console::{Style, Term};
use std::io::Write as _;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

struct UiState {
    verbosity: Verbosity,
    term: Term,
    is_tty: bool,
    last_progress_nanos: AtomicU64,
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

    let term = Term::stderr();
    let is_tty = term.is_term();

    let _ = UI.set(UiState {
        verbosity,
        term,
        is_tty,
        last_progress_nanos: AtomicU64::new(0),
    });
}

fn state() -> &'static UiState {
    UI.get_or_init(|| {
        let term = Term::stderr();
        let is_tty = term.is_term();
        UiState {
            verbosity: Verbosity::Normal,
            term,
            is_tty,
            last_progress_nanos: AtomicU64::new(0),
        }
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

/// Self-throttling in-place progress update.
/// Internally enforces 500ms minimum between updates.
/// Skips entirely on non-TTY to avoid ANSI garbage in log files.
pub fn transfer_progress(transferred: u64, start: &Instant) {
    let s = state();
    if s.verbosity == Verbosity::Quiet || !s.is_tty {
        return;
    }

    let elapsed = start.elapsed();
    let now_nanos = elapsed.as_nanos() as u64;
    let last = s.last_progress_nanos.load(Ordering::Relaxed);

    // Throttle: skip if less than 500ms since last update
    if now_nanos.saturating_sub(last) < 500_000_000 {
        return;
    }
    s.last_progress_nanos.store(now_nanos, Ordering::Relaxed);

    let elapsed_secs = elapsed.as_secs_f64();
    let speed = if elapsed_secs > 0.0 {
        (transferred as f64 / elapsed_secs) as u64
    } else {
        0
    };

    let content = format!(
        "    {} | {}/s",
        format_bytes(transferred),
        format_bytes(speed),
    );
    let style = Style::new().dim().cyan();
    // Single write: clear line + content in one call, no separate flush
    let line = format!("\x1b[2K\r{}", style.apply_to(&content));
    let _ = write!(&s.term, "{}", line);
    let _ = s.term.flush();
}

/// Finalize the progress line with final stats, replacing the live line.
/// TTY-aware: uses ANSI escape on TTY, plain line on non-TTY.
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
    let content = format!(
        "    {} transferred in {:.1}s ({}/s)",
        format_bytes(transferred),
        elapsed_secs,
        format_bytes(avg_speed),
    );

    if s.is_tty {
        let style = Style::new().dim().cyan();
        let line = format!("\x1b[2K\r{}", style.apply_to(&content));
        let _ = s.term.write_line(&line);
    } else {
        let _ = s.term.write_line(&content);
    }
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
