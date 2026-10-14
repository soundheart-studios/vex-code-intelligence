//! `vex init` at a terminal: ask which agent harness to wire to vex
//! (without `--agent`), instead of leaving it to a flag the person has to
//! know, and when a long embed is ahead, when to run it.

use std::io::{BufRead, IsTerminal, Write};
use vex_app::agent::Agent;
use vex_app::project::{EmbedChoice, EmbedEstimate};

/// Embeds expected to take less than this run without asking.
pub const ASK_EMBED_OVER: std::time::Duration = std::time::Duration::from_secs(120);

/// The embed menu, in order: (choice, label, what it means).
const EMBED_CHOICES: [(EmbedChoice, &str, &str); 3] = [
    (
        EmbedChoice::Now,
        "Now",
        "search is fully ready when this finishes",
    ),
    (
        EmbedChoice::Background,
        "In the background",
        "keyword search works now; meaning-based results fill in as the daemon catches up",
    ),
    (EmbedChoice::Later, "Later", "run `vex embed` when you like"),
];

/// The menu, in order: (label, agents).
const CHOICES: [(&str, &[Agent]); 4] = [
    ("Claude Code", &[Agent::Claude]),
    ("Codex", &[Agent::Codex]),
    ("Both", &[Agent::Claude, Agent::Codex]),
    ("Neither", &[]),
];

/// Whether to ask: a person at a terminal, no `--json`, no `--agent`, and
/// no harness already wired to vex in this project.
pub fn should_ask(json: bool, agents: &[Agent], root: Option<&std::path::Path>) -> bool {
    !json
        && agents.is_empty()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && root.is_none_or(|r| vex_app::agent::configured(r).is_empty())
}

/// Whether a long embed may be put to the person: at a terminal, no
/// `--json`.
pub fn should_ask_embed(json: bool) -> bool {
    !json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The menu entry offered as the default (0-based): what is installed.
pub(crate) fn default_choice(claude: bool, codex: bool) -> usize {
    match (claude, codex) {
        (true, true) => 2,
        (false, true) => 1,
        _ => 0,
    }
}

/// A reply to the menu: a number, a name, or empty for the default.
pub(crate) fn parse_choice(reply: &str, default: usize) -> Option<usize> {
    match reply.trim().to_ascii_lowercase().as_str() {
        "" => Some(default),
        "1" | "claude" | "claude code" => Some(0),
        "2" | "codex" => Some(1),
        "3" | "both" | "all" => Some(2),
        "4" | "neither" | "none" | "n" | "no" => Some(3),
        _ => None,
    }
}

/// Ask on stderr, read stdin; Neither on end of input or after three
/// replies that are not on the menu.
pub fn ask() -> Vec<Agent> {
    let default = default_choice(on_path("claude"), on_path("codex"));
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "Which agent should use vex in this project?");
    for (i, (label, _)) in CHOICES.iter().enumerate() {
        let _ = writeln!(err, "  {}) {label}", i + 1);
    }
    let stdin = std::io::stdin();
    for _ in 0..3 {
        let _ = write!(err, "Choice [{}]: ", default + 1);
        let _ = err.flush();
        let mut reply = String::new();
        if stdin.lock().read_line(&mut reply).unwrap_or(0) == 0 {
            let _ = writeln!(err);
            return Vec::new();
        }
        if let Some(i) = parse_choice(&reply, default) {
            return CHOICES[i].1.to_vec();
        }
        let _ = writeln!(err, "Type 1-4.");
    }
    Vec::new()
}

/// A reply to the embed menu: a number, a name, or empty for Background.
pub(crate) fn parse_embed_choice(reply: &str) -> Option<EmbedChoice> {
    match reply.trim().to_ascii_lowercase().as_str() {
        "1" | "now" => Some(EmbedChoice::Now),
        "" | "2" | "background" | "bg" => Some(EmbedChoice::Background),
        "3" | "later" => Some(EmbedChoice::Later),
        _ => None,
    }
}

/// Ask when to embed what a sample says will take long; Background on end
/// of input or after three replies that are not on the menu.
pub fn ask_embed(e: &EmbedEstimate) -> EmbedChoice {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "Embedding the remaining {} chunks will take {}.",
        e.pending,
        crate::cmd::progress::rough(e.estimate)
    );
    for (i, (_, label, meaning)) in EMBED_CHOICES.iter().enumerate() {
        let _ = writeln!(err, "  {}) {label:<18} {meaning}", i + 1);
    }
    let stdin = std::io::stdin();
    for _ in 0..3 {
        let _ = write!(err, "Choice [2]: ");
        let _ = err.flush();
        let mut reply = String::new();
        if stdin.lock().read_line(&mut reply).unwrap_or(0) == 0 {
            let _ = writeln!(err);
            return EmbedChoice::Background;
        }
        if let Some(c) = parse_embed_choice(&reply) {
            return c;
        }
        let _ = writeln!(err, "Type 1-3.");
    }
    EmbedChoice::Background
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_follows_what_is_installed() {
        assert_eq!(default_choice(true, false), 0);
        assert_eq!(default_choice(false, true), 1);
        assert_eq!(default_choice(true, true), 2);
        assert_eq!(default_choice(false, false), 0);
    }

    #[test]
    fn replies_by_number_name_or_enter() {
        assert_eq!(parse_choice("\n", 2), Some(2));
        assert_eq!(parse_choice(" 1 \n", 2), Some(0));
        assert_eq!(parse_choice("Codex", 0), Some(1));
        assert_eq!(parse_choice("both", 0), Some(2));
        assert_eq!(parse_choice("all", 0), Some(2));
        assert_eq!(parse_choice("4", 0), Some(3));
        assert_eq!(parse_choice("none", 0), Some(3));
        assert_eq!(parse_choice("5", 0), None);
        assert_eq!(parse_choice("yes", 0), None);
    }

    #[test]
    fn embed_replies_by_number_name_or_enter() {
        use vex_app::project::EmbedChoice::*;
        assert_eq!(parse_embed_choice("\n"), Some(Background));
        assert_eq!(parse_embed_choice("1"), Some(Now));
        assert_eq!(parse_embed_choice("now"), Some(Now));
        assert_eq!(parse_embed_choice(" 2 "), Some(Background));
        assert_eq!(parse_embed_choice("background"), Some(Background));
        assert_eq!(parse_embed_choice("3"), Some(Later));
        assert_eq!(parse_embed_choice("later"), Some(Later));
        assert_eq!(parse_embed_choice("4"), None);
    }

    #[test]
    fn the_menu_maps_to_agents() {
        assert_eq!(CHOICES[0].1, &[Agent::Claude]);
        assert_eq!(CHOICES[1].1, &[Agent::Codex]);
        assert_eq!(CHOICES[2].1, &[Agent::Claude, Agent::Codex]);
        assert!(CHOICES[3].1.is_empty());
    }
}
