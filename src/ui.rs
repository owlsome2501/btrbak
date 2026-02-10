use console::Style;
use std::io::{IsTerminal, Write};
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

    let _ = UI.set(UiState { verbosity, is_tty });
}

fn state() -> &'static UiState {
    UI.get_or_init(|| {
        let is_tty = std::io::stderr().is_terminal();
        console::set_colors_enabled(is_tty);
        let verbosity = if cfg!(test) {
            Verbosity::Quiet
        } else {
            Verbosity::Normal
        };
        UiState { verbosity, is_tty }
    })
}

/// Print a line to stderr.
fn println_stderr(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}

/// `\n== {msg} ==\n` (bold cyan)
pub fn header(msg: &str) {
    let s = state();
    if s.verbosity == Verbosity::Quiet {
        return;
    }
    let style = Style::new().bold().cyan();
    println_stderr(format!("\n{}\n", style.apply_to(format!("== {} ==", msg))));
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
    println_stderr(format!("  {}", style.apply_to(format!("\u{2713} {}", msg))));
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
    println_stderr(format!("  {}", style.apply_to(format!("\u{26a0} {}", msg))));
}

/// `  ✗ {msg}` (red) - always shown
pub fn error(msg: &str) {
    let style = Style::new().red();
    println_stderr(format!("  {}", style.apply_to(format!("\u{2717} {}", msg))));
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
    println_stderr(format!("  {}", style.apply_to(format!("$ {}", cmd_str))));
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

/// Format a duration in seconds into human-readable string
pub fn format_duration(secs: f64) -> String {
    let total_secs = secs as u64;
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let s = total_secs % 60;
        format!("{}h {}m {}s", hours, mins, s)
    } else if total_secs >= 60 {
        let mins = total_secs / 60;
        let s = total_secs % 60;
        format!("{}m {}s", mins, s)
    } else {
        format!("{:.1}s", secs)
    }
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
/// On TTY: shows live transfer stats on a single line using carriage return.
/// On non-TTY: progress hidden; final summary printed on `finish()`.
///
/// This simple approach avoids ANSI escape sequences that cause display
/// issues when the terminal is resized.
pub struct TransferProgress {
    show_progress: bool,
}

/// Create a new transfer progress display.
pub fn start_transfer() -> TransferProgress {
    let s = state();
    TransferProgress {
        show_progress: s.is_tty && s.verbosity != Verbosity::Quiet,
    }
}

impl TransferProgress {
    /// Update the progress display with current transfer stats.
    pub fn update(&self, transferred: u64, start: &Instant) {
        if self.show_progress {
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (transferred as f64 / elapsed) as u64
            } else {
                0
            };
            let style = Style::new().dim().cyan();
            let msg = format!(
                "    {} | {}/s",
                format_bytes(transferred),
                format_bytes(speed),
            );
            // Use carriage return to overwrite the current line
            // No complex ANSI sequences - just \r to move cursor to start
            let _ = write!(std::io::stderr(), "\r{}", style.apply_to(&msg));
            let _ = std::io::stderr().flush();
        }
    }

    /// Finalize the progress display with summary stats.
    pub fn finish(self, transferred: u64, elapsed_secs: f64) {
        let s = state();

        // Clear the progress line if we were showing one
        if self.show_progress {
            // Clear the current line using \r and spaces, then \r again
            eprint!("\r{:80}\r", "");
        }

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
        eprintln!("{}", style.apply_to(&content));
    }
}

impl Drop for TransferProgress {
    fn drop(&mut self) {
        // Clear the progress line if dropped without calling finish()
        if self.show_progress {
            eprint!("\r{:80}\r", "");
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    // format_duration tests

    #[test]
    fn test_format_duration_sub_second() {
        assert_eq!(format_duration(0.5), "0.5s");
    }

    #[test]
    fn test_format_duration_one_second() {
        assert_eq!(format_duration(1.0), "1.0s");
    }

    #[test]
    fn test_format_duration_seconds_only() {
        assert_eq!(format_duration(45.3), "45.3s");
    }

    #[test]
    fn test_format_duration_minutes_seconds() {
        assert_eq!(format_duration(150.0), "2m 30s");
    }

    #[test]
    fn test_format_duration_exact_minute() {
        assert_eq!(format_duration(60.0), "1m 0s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3723.0), "1h 2m 3s");
    }

    #[test]
    fn test_format_duration_exact_hour() {
        assert_eq!(format_duration(3600.0), "1h 0m 0s");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0.0), "0.0s");
    }

    // format_bytes tests

    #[test]
    fn test_format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn test_format_bytes_bytes() {
        assert_eq!(format_bytes(100), "100 B");
    }

    #[test]
    fn test_format_bytes_one_kib() {
        assert_eq!(format_bytes(1024), "1.0 KiB");
    }

    #[test]
    fn test_format_bytes_kib() {
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn test_format_bytes_mib() {
        assert_eq!(format_bytes(1_048_576), "1.00 MiB");
    }

    #[test]
    fn test_format_bytes_mib_fractional() {
        // 1.5 MiB = 1_572_864 bytes
        assert_eq!(format_bytes(1_572_864), "1.50 MiB");
    }

    #[test]
    fn test_format_bytes_gib() {
        assert_eq!(format_bytes(1_073_741_824), "1.00 GiB");
    }

    // format_cmd tests

    #[test]
    fn test_format_cmd_no_args() {
        let cmd = Command::new("ls");
        assert_eq!(format_cmd(&cmd), "ls");
    }

    #[test]
    fn test_format_cmd_simple_args() {
        let mut cmd = Command::new("btrfs");
        cmd.arg("subvolume").arg("show").arg("/mnt");
        assert_eq!(format_cmd(&cmd), "btrfs subvolume show /mnt");
    }

    #[test]
    fn test_format_cmd_args_with_spaces() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello world");
        assert_eq!(format_cmd(&cmd), "echo 'hello world'");
    }

    #[test]
    fn test_format_cmd_mixed_args() {
        let mut cmd = Command::new("cp");
        cmd.arg("/src/file").arg("/dest/path with spaces");
        assert_eq!(format_cmd(&cmd), "cp /src/file '/dest/path with spaces'");
    }

    #[test]
    fn test_format_cmd_multiple_space_args() {
        let mut cmd = Command::new("test");
        cmd.arg("arg one").arg("arg two");
        assert_eq!(format_cmd(&cmd), "test 'arg one' 'arg two'");
    }
}
