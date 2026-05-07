use std::io::{Stderr, Stdout, Write};
use std::sync::Mutex;

use anstream::AutoStream;
use anstyle::{AnsiColor, Effects, Style};

use crate::report::event::{BumpSource, Event, LeakDetail, TreeNode, TreeNodeKind};
use crate::report::sink::EventSink;
use crate::semver::{Bump, ChangeKind, format_name_set};

const fn fg(color: AnsiColor) -> Style {
    Style::new().fg_color(Some(anstyle::Color::Ansi(color)))
}

const RED: Style = fg(AnsiColor::Red);
const RED_BOLD: Style = RED.effects(Effects::BOLD);
const YELLOW: Style = fg(AnsiColor::Yellow);
const YELLOW_BOLD: Style = YELLOW.effects(Effects::BOLD);
const GREEN: Style = fg(AnsiColor::Green);
const GREEN_BOLD: Style = GREEN.effects(Effects::BOLD);
const CYAN: Style = fg(AnsiColor::Cyan);
const CYAN_BOLD: Style = CYAN.effects(Effects::BOLD);
const BOLD: Style = Style::new().effects(Effects::BOLD);
const DIMMED: Style = Style::new().effects(Effects::DIMMED);
const WHITE_BOLD: Style = fg(AnsiColor::White).effects(Effects::BOLD);

/// Streams structured events to `stdout`/`stderr` via anstream.
pub struct StreamSink {
    out: Mutex<AutoStream<Stdout>>,
    err: Mutex<AutoStream<Stderr>>,
}

impl StreamSink {
    /// Build a sink with the given color choice.
    pub fn new(choice: anstream::ColorChoice) -> Self {
        Self {
            out: Mutex::new(AutoStream::new(std::io::stdout(), choice)),
            err: Mutex::new(AutoStream::new(std::io::stderr(), choice)),
        }
    }
}

impl EventSink for StreamSink {
    fn emit(&self, event: &Event<'_>) {
        match event {
            Event::DirectModeBanner { seeds } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "{} assuming BREAKING change for {}\n",
                    styled("Direct mode:", BOLD),
                    styled(&format_name_set(*seeds), CYAN),
                );
            }
            Event::ComparingRefs { source, target } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "Comparing versions between {} and {}...\n",
                    styled(source, CYAN_BOLD),
                    styled(target, CYAN_BOLD),
                );
            }
            Event::NoChangesDetected => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "{}",
                    styled(
                        "No breaking/additive version changes detected. Nothing to propagate.",
                        GREEN,
                    ),
                );
            }
            Event::DetectedChangesHeader => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(w, "{}", styled("Detected version changes:", BOLD));
            }
            Event::DepVersionChanged {
                name,
                old,
                new,
                kind,
            } => {
                let Some(label) = change_kind_label(*kind) else {
                    return;
                };
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {}: {} -> {} {}",
                    styled("[dep]", DIMMED),
                    styled(name, CYAN),
                    styled(old, DIMMED),
                    styled(new, WHITE_BOLD),
                    label,
                );
            }
            Event::LocalPackageBump {
                name,
                old,
                new,
                kind,
                source,
            } => {
                let Some(label) = change_kind_label(*kind) else {
                    return;
                };
                let suffix = match source {
                    BumpSource::Package => String::new(),
                    BumpSource::Workspace => format!(" {}", styled("[workspace]", DIMMED)),
                };
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {}: {} -> {} {}{}",
                    styled("[local]", DIMMED),
                    styled(name, CYAN),
                    styled(&old.to_string(), DIMMED),
                    styled(&new.to_string(), WHITE_BOLD),
                    label,
                    suffix,
                );
            }
            Event::NewCrate { name } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {} {}",
                    styled("[new]", DIMMED),
                    styled(name, CYAN),
                    styled("(NEW CRATE)", GREEN_BOLD),
                );
            }
            Event::RemovedCrate { name } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {} {}",
                    styled("[removed]", DIMMED),
                    styled(name, CYAN),
                    styled("(REMOVED)", RED_BOLD),
                );
            }
            Event::GlobMemberSkipped { member } => {
                let mut w = self.err.lock().expect("sink stderr poisoned");
                let _ = writeln!(
                    w,
                    "Warning: skipping glob workspace member '{}' for version inheritance",
                    member,
                );
            }
            Event::BreakingSeeds { names } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "\n{} {}\n",
                    styled("Breaking seeds:", BOLD),
                    styled(&format_name_set(*names), RED),
                );
            }
            Event::AdditiveSeeds { names } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "{} {}\n",
                    styled("Additive seeds:", BOLD),
                    styled(&format_name_set(*names), YELLOW),
                );
            }
            Event::AnalyzingCrate { name, deps } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "Analyzing {} for public API exposure of {:?}",
                    styled(name, CYAN_BOLD),
                    deps,
                );
            }
            Event::BinaryCrateSkipped { name } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {} is binary-only, no public API to leak",
                    styled("->", DIMMED),
                    styled(name, CYAN),
                );
            }
            Event::LeakDetected {
                crate_name,
                dep,
                bump,
                details,
            } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(
                    w,
                    "  {} {} leaks {} ({}):",
                    styled("->", RED_BOLD),
                    styled(crate_name, RED_BOLD),
                    styled(dep, YELLOW),
                    bump,
                );
                for LeakDetail {
                    item_kind,
                    item_name,
                    leaked_types,
                } in *details
                {
                    let types_str = leaked_types.join(", ");
                    let _ = writeln!(
                        w,
                        "     {} {} — uses {}",
                        styled(item_kind, DIMMED),
                        styled(item_name, DIMMED),
                        styled(&types_str, DIMMED),
                    );
                }
            }
            Event::RustdocFailed {
                crate_name,
                error,
                conservative_bump,
            } => {
                let mut w = self.err.lock().expect("sink stderr poisoned");
                let _ = writeln!(
                    w,
                    "  {} rustdoc JSON generation failed for {}: {}\n  \
                     Conservatively assuming {} bump.",
                    styled("WARNING:", YELLOW_BOLD),
                    styled(crate_name, CYAN),
                    error,
                    conservative_bump,
                );
            }
            Event::InfluenceTreeHeader => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(w, "\n{}", styled("=== Influence Tree ===", GREEN_BOLD),);
            }
            Event::InfluenceTree { nodes } => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                write_tree(&mut *w, nodes);
                let _ = writeln!(w);
            }
            Event::AnalysisCompleteHeader => {
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(w, "{}", styled("=== Analysis Complete ===", GREEN_BOLD));
            }
            Event::BumpList { level, names } => {
                let (label, style) = match level {
                    Bump::Major => ("MAJOR-bump list (Requires MAJOR bump / ↑.0.0):", RED_BOLD),
                    Bump::Minor => (
                        "MINOR-bump list (Requires MINOR bump / x.↑.0):",
                        YELLOW_BOLD,
                    ),
                    Bump::Patch => ("PATCH-bump list (Requires PATCH bump / x.y.↑):", CYAN_BOLD),
                    Bump::None => return,
                };
                let mut w = self.out.lock().expect("sink stdout poisoned");
                let _ = writeln!(w, "{} {}", styled(label, style), format_name_set(*names));
            }
            Event::FailedRustdocSummary { names } => {
                let mut w = self.err.lock().expect("sink stderr poisoned");
                let _ = writeln!(
                    w,
                    "\n{} The following crates failed rustdoc JSON generation \
                     and were conservatively assumed breaking. Verify manually:\n  {}",
                    styled("WARNING:", YELLOW_BOLD),
                    format_name_set(*names),
                );
            }
            Event::UnderBumped { items } => {
                let mut w = self.err.lock().expect("sink stderr poisoned");
                let _ = writeln!(
                    w,
                    "\n{} These crates have insufficient version bumps:",
                    styled("ERROR:", RED_BOLD),
                );
                for item in *items {
                    let _ = writeln!(
                        w,
                        "  {} has {} bump but requires {}",
                        styled(item.name, CYAN),
                        item.local,
                        item.required,
                    );
                }
            }
            Event::MissingBumps { items } => {
                let mut w = self.err.lock().expect("sink stderr poisoned");
                let _ = writeln!(
                    w,
                    "\n{} These crates need a version bump but have none:",
                    styled("ERROR:", RED_BOLD),
                );
                for item in *items {
                    let _ = writeln!(
                        w,
                        "  {} requires {}",
                        styled(item.name, CYAN),
                        item.required,
                    );
                }
            }
        }
    }
}

/// Render a styled fragment with anstyle's display reset.
fn styled<'a, T: std::fmt::Display + ?Sized>(text: &'a T, style: Style) -> Styled<'a, T> {
    Styled { text, style }
}

struct Styled<'a, T: std::fmt::Display + ?Sized> {
    text: &'a T,
    style: Style,
}

impl<T: std::fmt::Display + ?Sized> std::fmt::Display for Styled<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.style.render(),
            self.text,
            self.style.render_reset()
        )
    }
}

/// Map a `ChangeKind` to its styled label fragment.
fn change_kind_label(kind: ChangeKind) -> Option<String> {
    match kind {
        ChangeKind::Breaking => Some(styled("(BREAKING)", RED_BOLD).to_string()),
        ChangeKind::Additive => Some(styled("(ADDITIVE)", YELLOW_BOLD).to_string()),
        ChangeKind::Patch => Some(styled("(PATCH)", GREEN).to_string()),
        ChangeKind::None => None,
    }
}

fn write_tree<W: Write>(w: &mut W, nodes: &[TreeNode]) {
    let mut last_at_depth: Vec<bool> = Vec::new();
    for node in nodes {
        last_at_depth.truncate(node.depth);
        last_at_depth.push(node.is_last_sibling);
        let parent_prefix = ancestor_prefix(&last_at_depth);
        let connector = if node.is_last_sibling {
            "└── "
        } else {
            "├── "
        };
        match &node.kind {
            TreeNodeKind::Seed => {
                let _ = writeln!(
                    w,
                    "{}{}",
                    styled(connector, DIMMED),
                    styled(&format!("{} (seed)", node.name), YELLOW_BOLD),
                );
            }
            TreeNodeKind::Child {
                bump,
                already_shown,
            } => {
                let (connector_style, bump_label) = bump_styles(*bump);
                if *already_shown {
                    let _ = writeln!(
                        w,
                        "{}{}{} {}",
                        styled(&parent_prefix, DIMMED),
                        styled(connector, connector_style),
                        styled(&node.name, CYAN),
                        styled(&format!("({}, already shown above)", bump_label), DIMMED,),
                    );
                } else {
                    let _ = writeln!(
                        w,
                        "{}{}{}  ({})",
                        styled(&parent_prefix, DIMMED),
                        styled(connector, connector_style),
                        styled(&node.name, CYAN_BOLD),
                        bump_label,
                    );
                }
            }
        }
    }
}

/// Build the prefix string for the ancestors of the current node.
fn ancestor_prefix(last_at_depth: &[bool]) -> String {
    if last_at_depth.len() <= 1 {
        return String::new();
    }
    let mut s = String::new();
    for is_last in &last_at_depth[..last_at_depth.len() - 1] {
        s.push_str(if *is_last { "    " } else { "│   " });
    }
    s
}

fn bump_styles(bump: Bump) -> (Style, String) {
    match bump {
        Bump::Major => (RED_BOLD, styled("MAJOR", RED_BOLD).to_string()),
        Bump::Minor => (RED_BOLD, styled("MINOR", RED_BOLD).to_string()),
        Bump::Patch => (GREEN, styled("PATCH", GREEN).to_string()),
        Bump::None => (DIMMED, styled("none", DIMMED).to_string()),
    }
}
