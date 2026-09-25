//! One help layout for the complete public command tree.

use clap::builder::styling::{AnsiColor, Styles};
use clap::{ColorChoice, Command};

use crate::presentation::{terminal_width, wrap};

const HEADING: clap::builder::styling::Style = AnsiColor::Cyan.on_default().bold();
const LITERAL: clap::builder::styling::Style = clap::builder::styling::Style::new().bold();

pub fn configure(command: Command, color: ColorChoice) -> Command {
    let width = terminal_width();
    let introduction = introduction(&command, width);
    command
        .styles(
            Styles::styled()
                .header(HEADING)
                .usage(HEADING)
                .literal(LITERAL)
                .placeholder(AnsiColor::Cyan.on_default())
                .error(AnsiColor::Red.on_default().bold()),
        )
        .color(color)
        .term_width(width)
        .before_help(introduction)
        .help_template("{before-help}\n{usage-heading} {usage}\n\nOptions (all commands):\n{options}\n{after-help}")
}

const GROUPS: &[(&str, &[&str])] = &[
    ("Find packages", &["search", "info"]),
    (
        "Manage packages",
        &[
            "install", "list", "remove", "outdated", "update", "upgrade", "pin", "unpin",
        ],
    ),
    (
        "Manage your environment",
        &["history", "rollback", "gc", "repair"],
    ),
    (
        "Set up and diagnose",
        &["doctor", "shellenv", "completion", "uninstall"],
    ),
];

fn introduction(command: &Command, width: usize) -> String {
    let mut text = format!(
        "{HEADING}pkg{HEADING:#}  {}\nFind, install, and manage command-line tools.\n",
        env!("CARGO_PKG_VERSION")
    );
    for &(heading, names) in GROUPS {
        text.push_str(&format!("\n{HEADING}{heading}{HEADING:#}\n"));
        for name in names {
            if let Some(subcommand) = command.find_subcommand(name) {
                let about = subcommand
                    .get_about()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                if width < 60 {
                    text.push_str(&format!("  {LITERAL}{name}{LITERAL:#}\n"));
                    for line in wrap(&about, width.saturating_sub(4)) {
                        text.push_str(&format!("    {line}\n"));
                    }
                } else {
                    let lines = wrap(&about, width.saturating_sub(15));
                    text.push_str(&format!("  {LITERAL}{name:<11}{LITERAL:#}  {}\n", lines[0]));
                    for line in &lines[1..] {
                        text.push_str(&format!("               {line}\n"));
                    }
                }
            }
        }
    }
    text
}
