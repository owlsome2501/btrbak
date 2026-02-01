use console::Style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

struct UiState {
    verbosity: Verbosity,
    is_tty: bool,
    mp: MultiProgress,
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

    let is_tty = std::io::stderr().is_terminal();

    // All output goes to stderr; set color support based on stderr TTY detection
    console::set_colors_enabled(is_tty);

    let _ = UI.set(UiState {
        verbosity,
        is_tty,
        mp: MultiProgress::new(),
    });
}

fn state() -> &'static UiState {
    UI.get_or_init(|| {
        let is_tty = std::io::stderr().is_terminal();
        console::set_colors_enabled(is_tty);
        UiState {
            verbosity: Verbosity::Normal,
            is_tty,
            mp: MultiProgress::new(),
        }
    })
}

/// Print a line to stderr, coordinated with any active progress bars.
/// Uses `MultiProgress::println` which clears active progress bars before
/// printing, then redraws them — preventing garbled output.
fn println_stderr(msg: impl AsRef<str>) {
    let s = state();
    let _ = s.mp.println(msg);
}

/// `\n== {msg} ==\n` (bold cyan)
pub fn header(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().bold().cyan();
    println_stderr(format!(
        "\n{}\n",
        style.apply_to(format!("== {} ==", msg))
    ));
}

/// `[{cur}/{total}] {msg}` (bold)
pub fn step(cur: usize, total: usize, msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().bold();
    println_stderr(format!(
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
    println_stderr(format!("  {} {}", style.apply_to("->"), msg));
}

/// `  ✓ {msg}` (green)
pub fn success(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().green();
    println_stderr(format!(
        "  {}",
        style.apply_to(format!("\u{2713} {}", msg))
    ));
}

/// `  {msg}`
pub fn info(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    println_stderr(format!("  {}", msg));
}

/// `     {msg}` (dim) - only in verbose mode
pub fn detail(msg: &str) {
    let s = state();
    if s.verbosity != Verbosity::Verbose {
        return;
    }
    let style = Style::new().dim();
    println_stderr(format!("     {}", style.apply_to(msg)));
}

/// `  ⚠ {msg}` (yellow) - always shown
pub fn warning(msg: &str) {
    let style = Style::new().yellow();
    println_stderr(format!(
        "  {}",
        style.apply_to(format!("\u{26a0} {}", msg))
    ));
}

/// `  ✗ {msg}` (red) - always shown
pub fn error(msg: &str) {
    let style = Style::new().red();
    println_stderr(format!(
        "  {}",
        style.apply_to(format!("\u{2717} {}", msg))
    ));
}

/// Error with indented hint lines - always shown
pub fn error_with_hints(msg: &str, hints: &[&str]) {
    error(msg);
    if !hints.is_empty() {
        println_stderr("");
        let style = Style::new().dim();
        println_stderr(format!("  {}", style.apply_to("Hints:")));
        for hint in hints {
            println_stderr(format!(
                "    {}",
                style.apply_to(format!("\u{2022} {}", hint))
            ));
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
    println_stderr(format!(
        "  {}",
        style.apply_to(format!("$ {}", cmd_str))
    ));
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
            println_stderr(format!(
                "    {}",
                style.apply_to(format!("stderr: {}", trimmed))
            ));
        }
    }
}

/// Print a blank line as section separator
pub fn section_end() {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    println_stderr("");
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

/// Handle for an active transfer progress display.
///
/// Uses `indicatif::ProgressBar` managed by `MultiProgress`, so all output
/// (progress + text messages) is properly coordinated without garbled lines.
///
/// On TTY: animated spinner with live transfer stats.
/// On non-TTY: progress hidden; final summary printed on `finish()`.
///
/// Implements `Drop` to ensure the progress bar is cleared on error paths.
pub struct TransferProgress {
    bar: ProgressBar,
}

/// Create a new transfer progress display.
pub fn start_transfer() -> TransferProgress {
    let s = state();
    let bar = if s.is_tty && s.verbosity != Verbosity::Quiet {
        let bar = s.mp.add(ProgressBar::new_spinner());
        bar.set_style(
            ProgressStyle::with_template("    {spinner:.cyan.dim} {msg:.cyan.dim}")
                .unwrap(),
        );
        bar.enable_steady_tick(std::time::Duration::from_millis(100));
        bar
    } else {
        ProgressBar::with_draw_target(None, ProgressDrawTarget::hidden())
    };
    TransferProgress { bar }
}

impl TransferProgress {
    /// Update the progress display with current transfer stats.
    pub fn update(&self, transferred: u64, start: &Instant) {
        let elapsed = start.elapsed().as_secs_f64();
        let speed = if elapsed > 0.0 {
            (transferred as f64 / elapsed) as u64
        } else {
            0
        };
        self.bar.set_message(format!(
            "{} | {}/s",
            format_bytes(transferred),
            format_bytes(speed),
        ));
    }

    /// Finalize the progress display with summary stats.
    pub fn finish(self, transferred: u64, elapsed_secs: f64) {
        self.bar.finish_and_clear();

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
        let style = Style::new().dim().cyan();
        println_stderr(format!("{}", style.apply_to(&content)));
    }
}

impl Drop for TransferProgress {
    fn drop(&mut self) {
        // Ensure the progress bar is cleared if dropped without calling finish()
        // (e.g. on error paths). No-op if already finished.
        self.bar.finish_and_clear();
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
