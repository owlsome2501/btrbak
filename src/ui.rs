use console::{Style, Term};
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
