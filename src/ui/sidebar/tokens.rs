use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SidebarTokenColor, SidebarTokenStyle,
    SpaceSidebarToken, SpacesSidebarConfig,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToken {
    pub kind: ResolvedTokenKind,
    pub style: SidebarTokenStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTokenKind {
    StateIcon,
    StateText(String),
    Machine(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus {
        ahead: usize,
        behind: usize,
    },
    Custom(String),
    /// Background work drawn in the agent's own glyphs and colors.
    Activity(Vec<ActivityPart>),
}

/// One piece of an agent's background work. `kind`, `count` and `label` are
/// the data a front end draws its own icon from; `text` and `color` are how
/// the TUI draws it, in Claude Code's glyphs and colors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityPart {
    pub kind: ActivityKind,
    /// How many: shells, monitors, subagents or MCP tasks; 1 for a goal or loop.
    pub count: u16,
    /// A loop's next wakeup (`22:42`) or interval (`2m`), when known.
    pub label: Option<String>,
    pub text: String,
    pub color: SidebarTokenColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActivityKind {
    Goal,
    Loop,
    Shells,
    Monitors,
    Subagents,
    McpTasks,
    /// The agent itself, as a small mark. Front ends with their own icons can
    /// draw from `AgentEntry::agent` instead.
    Agent,
}

/// What keeps an agent going between turns. The state icon of a pane with
/// one takes the mode's color, and the mode's own glyph while it works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AgentMode {
    Goal,
    Loop,
}

impl AgentMode {
    /// A goal leads when a pane runs both.
    pub(crate) fn from_tokens(tokens: &std::collections::HashMap<String, String>) -> Option<Self> {
        if tokens.contains_key("oasis:goal") {
            Some(Self::Goal)
        } else if tokens.contains_key("oasis:loop") || tokens.contains_key("oasis:loop_next") {
            Some(Self::Loop)
        } else {
            None
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Goal => "◎",
            Self::Loop => "↻",
        }
    }

    pub fn color(self) -> SidebarTokenColor {
        match self {
            Self::Goal => GOAL_COLOR,
            Self::Loop => LOOP_COLOR,
        }
    }
}

impl ResolvedTokenKind {
    fn text_value(&self) -> Option<&str> {
        match self {
            Self::StateText(value)
            | Self::Machine(value)
            | Self::Workspace(value)
            | Self::Tab(value)
            | Self::Pane(value)
            | Self::Agent(value)
            | Self::TerminalTitle(value)
            | Self::Branch(value)
            | Self::Custom(value) => Some(value),
            Self::StateIcon | Self::GitStatus { .. } | Self::Activity(_) => None,
        }
    }
}

impl ResolvedToken {
    fn new(kind: ResolvedTokenKind, style: SidebarTokenStyle) -> Self {
        Self { kind, style }
    }

    #[cfg(test)]
    pub(super) fn unstyled(kind: ResolvedTokenKind) -> Self {
        Self::new(kind, SidebarTokenStyle::default())
    }
}

pub(crate) struct AgentTokenContext<'a> {
    pub(crate) machine: Option<&'a str>,
    pub(crate) workspace: &'a str,
    pub(crate) tab: Option<&'a str>,
    pub(crate) pane: Option<&'a str>,
    pub(crate) agent_label: Option<&'a str>,
    pub(crate) terminal_title: Option<&'a str>,
    pub(crate) terminal_title_stripped: Option<&'a str>,
    pub(crate) canonical_agent: Option<crate::detect::Agent>,
    pub(crate) tokens: &'a std::collections::HashMap<String, String>,
}

pub(crate) fn agent_rows(
    config: &AgentsSidebarConfig,
    context: AgentTokenContext<'_>,
    state_text: &str,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(context.canonical_agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        AgentSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        AgentSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(state_text.to_string()))
                        }
                        AgentSidebarToken::Machine => context
                            .machine
                            .map(|value| ResolvedTokenKind::Machine(value.to_string())),
                        AgentSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        AgentSidebarToken::Tab => context
                            .tab
                            .map(|value| ResolvedTokenKind::Tab(value.to_string())),
                        AgentSidebarToken::Pane => context
                            .pane
                            .map(|value| ResolvedTokenKind::Pane(value.to_string())),
                        AgentSidebarToken::Agent => context
                            .agent_label
                            .map(|value| ResolvedTokenKind::Agent(value.to_string())),
                        AgentSidebarToken::TerminalTitle => context
                            .terminal_title
                            .map(|value| ResolvedTokenKind::TerminalTitle(value.to_string())),
                        AgentSidebarToken::TerminalTitleStripped => context
                            .terminal_title_stripped
                            .map(|value| ResolvedTokenKind::TerminalTitle(value.to_string())),
                        AgentSidebarToken::Background => activity(background_parts(context.tokens)),
                        AgentSidebarToken::Shells => activity(shells_part(context.tokens)),
                        AgentSidebarToken::Monitors => activity(monitors_part(context.tokens)),
                        AgentSidebarToken::Subagents => activity(subagents_part(context.tokens)),
                        AgentSidebarToken::Wakeup => activity(loop_part(context.tokens)),
                        AgentSidebarToken::Goal => activity(goal_part(context.tokens)),
                        AgentSidebarToken::AgentIcon => activity(agent_icon_part(
                            context.canonical_agent,
                            context.agent_label,
                        )),
                        AgentSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        AgentSidebarToken::Styled { .. } => None,
                    }?;
                    let style = kind
                        .text_value()
                        .map_or(Some(style), |value| configured.style_for_value(value))?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

// Claude Code's colors, read from live panes: its mode line draws the
// `1 shell, 1 monitor` counts in cyan and a grey `◯` row per subagent, and its
// goal notice is `◎`. Goal pink (256-color 213) and loop green (114) follow the
// statusline these were first read from, as stock Claude draws no loop at all.
// Idle agents are grey, so loop green stands alone.
// Claude has no glyph for shells, monitors or MCP tasks, so those are short
// counts in its cyan.
const GOAL_COLOR: SidebarTokenColor = SidebarTokenColor::from_rgb(0xff, 0x87, 0xff);
const LOOP_COLOR: SidebarTokenColor = SidebarTokenColor::from_rgb(0x87, 0xd7, 0x87);
const COUNT_COLOR: SidebarTokenColor = SidebarTokenColor::from_rgb(0, 204, 204);
const SUBAGENT_COLOR: SidebarTokenColor = SidebarTokenColor::from_rgb(153, 153, 153);

// One-cell marks for agents, so a row can name its agent without the word.
// Claude's is the ✳ Claude Code puts in its own window title; the rest are
// plain symbols in a color close to each tool's own. Agents without one get
// their first letter in a color picked from their name, so each stays stable.
const CLAUDE_COLOR: SidebarTokenColor = SidebarTokenColor::from_rgb(0xd9, 0x77, 0x57);
const AGENT_LETTER_COLORS: [SidebarTokenColor; 6] = [
    SidebarTokenColor::from_rgb(0xe5, 0xc0, 0x7b),
    SidebarTokenColor::from_rgb(0x61, 0xaf, 0xef),
    SidebarTokenColor::from_rgb(0x98, 0xc3, 0x79),
    SidebarTokenColor::from_rgb(0xc6, 0x78, 0xdd),
    SidebarTokenColor::from_rgb(0x56, 0xb6, 0xc2),
    SidebarTokenColor::from_rgb(0xe0, 0x6c, 0x75),
];

fn agent_icon_part(
    agent: Option<crate::detect::Agent>,
    label: Option<&str>,
) -> Option<ActivityPart> {
    use crate::detect::Agent;
    let mark = |glyph: &str, r, g, b| (glyph.to_owned(), SidebarTokenColor::from_rgb(r, g, b));
    let (text, color) = match agent {
        Some(Agent::Claude) => ("✳".to_owned(), CLAUDE_COLOR),
        Some(Agent::Codex) => mark("◇", 0x10, 0xa3, 0x7f),
        Some(Agent::Gemini) => mark("✦", 0x42, 0x85, 0xf4),
        Some(Agent::Cursor) => mark("◆", 0x9b, 0x8c, 0xff),
        Some(Agent::OpenCode) => mark("◈", 0xf5, 0xa7, 0x42),
        Some(Agent::GithubCopilot) => mark("◉", 0x89, 0x57, 0xe5),
        Some(Agent::Pi) => mark("π", 0xc6, 0x78, 0xdd),
        _ => {
            let name = label?;
            let letter = name.chars().next()?.to_uppercase().collect::<String>();
            let index = name.bytes().fold(0usize, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(usize::from(byte))
            }) % AGENT_LETTER_COLORS.len();
            (letter, AGENT_LETTER_COLORS[index])
        }
    };
    Some(ActivityPart {
        kind: ActivityKind::Agent,
        count: 1,
        label: None,
        text,
        color,
    })
}

fn activity(parts: impl IntoIterator<Item = ActivityPart>) -> Option<ResolvedTokenKind> {
    let parts = parts.into_iter().collect::<Vec<_>>();
    (!parts.is_empty()).then_some(ResolvedTokenKind::Activity(parts))
}

/// The reserved `oasis:<key>` count the server attaches.
fn count(tokens: &std::collections::HashMap<String, String>, key: &str) -> Option<u16> {
    tokens
        .get(&format!("oasis:{key}"))?
        .parse()
        .ok()
        .filter(|count| *count > 0)
}

fn counted_part(
    tokens: &std::collections::HashMap<String, String>,
    key: &str,
    kind: ActivityKind,
    text: impl Fn(u16) -> String,
) -> Option<ActivityPart> {
    count(tokens, key).map(|count| ActivityPart {
        kind,
        count,
        label: None,
        text: text(count),
        color: COUNT_COLOR,
    })
}

fn goal_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    tokens.contains_key("oasis:goal").then(|| ActivityPart {
        kind: ActivityKind::Goal,
        count: 1,
        label: None,
        text: AgentMode::Goal.glyph().to_owned(),
        color: GOAL_COLOR,
    })
}

/// `oasis:loop` holds the next wakeup or interval, empty when unknown;
/// servers before it only sent `oasis:loop_next`.
fn loop_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    let label = tokens
        .get("oasis:loop")
        .or_else(|| tokens.get("oasis:loop_next"))?;
    Some(ActivityPart {
        kind: ActivityKind::Loop,
        count: 1,
        label: (!label.is_empty()).then(|| label.clone()),
        text: if label.is_empty() {
            "↻".to_owned()
        } else {
            format!("↻ {label}")
        },
        color: LOOP_COLOR,
    })
}

fn shells_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    // A leading mark, so counts don't read as durations (`5s`, `1m`).
    counted_part(tokens, "shells", ActivityKind::Shells, |count| {
        format!("${count}")
    })
}

fn monitors_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    counted_part(tokens, "monitors", ActivityKind::Monitors, |count| {
        format!("∿{count}")
    })
}

fn mcp_tasks_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    counted_part(tokens, "mcp_tasks", ActivityKind::McpTasks, |count| {
        format!("{count}mcp")
    })
}

/// One `◯` per subagent, as Claude lists a row for each.
fn subagents_part(tokens: &std::collections::HashMap<String, String>) -> Option<ActivityPart> {
    count(tokens, "agents").map(|count| ActivityPart {
        kind: ActivityKind::Subagents,
        count,
        label: None,
        text: "◯".repeat(usize::from(count)),
        color: SUBAGENT_COLOR,
    })
}

/// The goal or loop itself shows in the state icon and the row's colors, so it
/// adds nothing here; the `wakeup` token still shows a loop's next run. While
/// either drives the pane, its shells, monitors and MCP tasks are the plumbing
/// it runs on and stay hidden; subagents stay.
fn background_parts(tokens: &std::collections::HashMap<String, String>) -> Vec<ActivityPart> {
    let driven = AgentMode::from_tokens(tokens).is_some();
    let plumbing = (!driven)
        .then(|| {
            [
                shells_part(tokens),
                monitors_part(tokens),
                mcp_tasks_part(tokens),
            ]
        })
        .into_iter()
        .flatten();
    plumbing.chain([subagents_part(tokens)]).flatten().collect()
}

pub(crate) struct SpaceTokenContext<'a> {
    pub(crate) workspace: &'a str,
    pub(crate) branch: Option<&'a str>,
    pub(crate) state_text: &'a str,
    pub(crate) ahead_behind: Option<(usize, usize)>,
    pub(crate) tokens: &'a std::collections::HashMap<String, String>,
    pub(crate) suppress_git_details: bool,
}

pub(crate) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|configured| {
                    let (token, style) = configured.parts();
                    let kind = match token {
                        SpaceSidebarToken::StateIcon => Some(ResolvedTokenKind::StateIcon),
                        SpaceSidebarToken::StateText => {
                            Some(ResolvedTokenKind::StateText(context.state_text.to_string()))
                        }
                        SpaceSidebarToken::Workspace => {
                            Some(ResolvedTokenKind::Workspace(context.workspace.to_string()))
                        }
                        SpaceSidebarToken::Branch if !context.suppress_git_details => context
                            .branch
                            .map(|branch| ResolvedTokenKind::Branch(branch.to_string())),
                        SpaceSidebarToken::Branch => None,
                        SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                            .ahead_behind
                            .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                            .map(|(ahead, behind)| ResolvedTokenKind::GitStatus { ahead, behind }),
                        SpaceSidebarToken::GitStatus => None,
                        SpaceSidebarToken::Custom(name) => context
                            .tokens
                            .get(name)
                            .cloned()
                            .map(ResolvedTokenKind::Custom),
                        SpaceSidebarToken::Styled { .. } => None,
                    }?;
                    let style = kind
                        .text_value()
                        .map_or(Some(style), |value| configured.style_for_value(value))?;
                    Some(ResolvedToken::new(kind, style))
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(crate) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    if matches!(previous.kind, ResolvedTokenKind::StateIcon)
        || matches!(current.kind, ResolvedTokenKind::GitStatus { .. })
    {
        " "
    } else {
        " · "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentSidebarToken, SpaceSidebarToken};

    struct Entry {
        workspace: String,
        tab: Option<String>,
        pane: Option<String>,
        agent_label: Option<String>,
        terminal_title: Option<String>,
        terminal_title_stripped: Option<String>,
        canonical_agent: Option<crate::detect::Agent>,
        tokens: std::collections::HashMap<String, String>,
    }

    fn entry() -> Entry {
        Entry {
            workspace: "repo".into(),
            tab: None,
            pane: None,
            agent_label: Some("pi".into()),
            terminal_title: None,
            terminal_title_stripped: None,
            canonical_agent: Some(crate::detect::Agent::Pi),
            tokens: std::collections::HashMap::new(),
        }
    }

    fn context(entry: &Entry) -> AgentTokenContext<'_> {
        AgentTokenContext {
            machine: None,
            workspace: &entry.workspace,
            tab: entry.tab.as_deref(),
            pane: entry.pane.as_deref(),
            agent_label: entry.agent_label.as_deref(),
            terminal_title: entry.terminal_title.as_deref(),
            terminal_title_stripped: entry.terminal_title_stripped.as_deref(),
            canonical_agent: entry.canonical_agent,
            tokens: &entry.tokens,
        }
    }

    #[test]
    fn conditional_styles_merge_first_match_and_keep_missing_values_absent() {
        let config: AgentsSidebarConfig = toml::from_str(r##"
rows = [["state_icon", { token = "machine", fg = "#fff", bold = true, dim = true, rules = [{ equals = "Local", fg = "#f00", bold = false }, { contains = "Loc", fg = "#0f0", dim = false }] }], [{ token = "$missing", rules = [{ equals = "", bold = true }] }]]
"##).unwrap();
        let entry = entry();
        for (machine, color, bold, dim) in [
            ("Local", (255, 0, 0), false, true),
            ("Localhost", (0, 255, 0), true, false),
            ("local", (255, 255, 255), true, true),
        ] {
            let mut context = context(&entry);
            context.machine = Some(machine);
            let rows = agent_rows(&config, context, "working");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0][0],
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon)
            );
            let token = &rows[0][1];
            assert_eq!(token.kind, ResolvedTokenKind::Machine(machine.into()));
            assert_eq!(
                token.style.fg.unwrap().ratatui(),
                ratatui::style::Color::Rgb(color.0, color.1, color.2)
            );
            assert_eq!(token.style.bold, Some(bold));
            assert_eq!(token.style.dim, Some(dim));
        }
        assert_eq!(agent_rows(&config, context(&entry), "working")[0].len(), 1);
    }

    #[test]
    fn conditional_style_survives_truncation_and_removes_theme_modifiers() {
        use ratatui::style::{Color, Modifier, Style};
        let config: AgentsSidebarConfig = toml::from_str(r##"
rows = [[{ token = "workspace", rules = [{ equals = "long-workspace-name", fg = "#f00", bold = false, dim = false }] }]]
"##).unwrap();
        let mut entry = entry();
        entry.workspace = "long-workspace-name".into();
        let rows = agent_rows(&config, context(&entry), "working");
        let theme = Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD | Modifier::DIM);
        for width in [4, 40] {
            let spans = super::super::resolved_token_spans(
                &rows[0],
                ("*", theme),
                theme,
                theme,
                theme,
                theme,
                &super::super::Palette::catppuccin(),
                width,
            );
            assert_eq!(spans.len(), 1);
            assert!(super::super::display_width(&spans[0].content) <= width);
            assert_eq!(spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
            assert!(!spans[0]
                .style
                .add_modifier
                .intersects(Modifier::BOLD | Modifier::DIM));
            assert!(spans[0]
                .style
                .sub_modifier
                .contains(Modifier::BOLD | Modifier::DIM));
        }
    }

    #[test]
    fn custom_numeric_rules_resolve_in_agent_overrides_and_space_rows() {
        let config: crate::config::SidebarConfig = toml::from_str(
            r#"
[agents]
rows = [["workspace"]]
[agents.rows_by_agent]
pi = [[{ token = "$load", rules = [{ gt = 80, bold = true }, { gt = 50, dim = true }] }]]
[spaces]
rows = [[{ token = "$load", rules = [{ lt = 50, dim = true }] }]]
"#,
        )
        .unwrap();
        let mut entry = entry();
        for (value, bold, dim) in [
            ("90", Some(true), None),
            ("60", None, Some(true)),
            ("20", None, None),
            ("90%", None, None),
        ] {
            entry.tokens.insert("load".into(), value.into());
            let rows = agent_rows(&config.agents, context(&entry), "working");
            assert_eq!(rows[0][0].kind, ResolvedTokenKind::Custom(value.into()));
            assert_eq!(rows[0][0].style.bold, bold);
            assert_eq!(rows[0][0].style.dim, dim);
            let spaces = space_rows(
                &config.spaces,
                SpaceTokenContext {
                    workspace: "repo",
                    branch: None,
                    state_text: "working",
                    ahead_behind: None,
                    suppress_git_details: false,
                    tokens: &entry.tokens,
                },
            );
            assert_eq!(spaces[0][0].style.dim, (value == "20").then_some(true));
        }
    }

    #[test]
    fn conditional_hide_removes_tokens_and_empty_rows() {
        let config: crate::config::SidebarConfig = toml::from_str(
            r##"
[agents]
rows = [[{ token = "machine", fg = "#61afef", rules = [{ equals = "Local", hide = true }] }, "agent"]]
[agents.rows_by_agent]
pi = [[{ token = "$load", rules = [{ lt = 50, hide = true }] }], ["agent"]]
[spaces]
rows = [[{ token = "$load", rules = [{ lt = 50, hide = true }] }], ["workspace"]]
"##,
        ).unwrap();
        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("hide = true"));
        let config: crate::config::SidebarConfig = toml::from_str(&encoded).unwrap();
        let mut entry = entry();
        entry.canonical_agent = None;
        for (machine, count) in [("Local", 1), ("Remote", 2)] {
            let mut ctx = context(&entry);
            ctx.machine = Some(machine);
            let rows = agent_rows(&config.agents, ctx, "working");
            assert_eq!(rows[0].len(), count);
            assert_eq!(
                rows[0].last().unwrap().kind,
                ResolvedTokenKind::Agent("pi".into())
            );
        }
        entry.canonical_agent = Some(crate::detect::Agent::Pi);
        for (value, count) in [("20", 1), ("90", 2)] {
            entry.tokens.insert("load".into(), value.into());
            assert_eq!(
                agent_rows(&config.agents, context(&entry), "working").len(),
                count
            );
            let rows = space_rows(
                &config.spaces,
                SpaceTokenContext {
                    workspace: "repo",
                    branch: None,
                    state_text: "working",
                    ahead_behind: None,
                    suppress_git_details: false,
                    tokens: &entry.tokens,
                },
            );
            assert_eq!(rows.len(), count);
        }
    }

    #[test]
    fn conditional_hide_preserves_first_match_wins() {
        for first in ["hide = false", "bold = true"] {
            let config: AgentsSidebarConfig = toml::from_str(&format!(
                "rows = [[{{ token = 'agent', rules = [{{ equals = 'pi', {first} }}, {{ contains = '', hide = true }}] }}]]"
            )).unwrap();
            let entry = entry();
            let rows = agent_rows(&config, context(&entry), "working");
            assert_eq!(rows[0][0].kind, ResolvedTokenKind::Agent("pi".into()));
        }
    }

    #[test]
    fn missing_custom_tokens_elide_rows_and_separators() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Custom("missing".into()),
                ],
                vec![AgentSidebarToken::Custom("missing".into())],
                vec![AgentSidebarToken::Agent],
            ],
            ..Default::default()
        };

        let rows = agent_rows(&config, context(&entry), "working");

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::StateIcon)]
        );
        assert_eq!(
            rows[1],
            vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "pi".into()
            ))]
        );
    }

    #[test]
    fn machine_token_only_resolves_for_multi_machine_rows() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::Machine,
                AgentSidebarToken::Workspace,
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, context(&entry), "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );

        let mut remote_context = context(&entry);
        remote_context.machine = Some("Build");
        assert_eq!(
            agent_rows(&config, remote_context, "working"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::Machine("Build".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("repo".into())),
            ]]
        );
    }

    #[test]
    fn state_text_and_arbitrary_values_are_independent_tokens() {
        let mut entry = entry();
        entry
            .tokens
            .insert("summary".into(), "reviewing auth".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Custom("summary".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, context(&entry), "deep in the mines"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateText("deep in the mines".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("reviewing auth".into())),
            ]]
        );
    }

    #[test]
    fn terminal_title_builtins_are_distinct_from_custom_tokens() {
        let mut entry = entry();
        entry.terminal_title = Some("⠋ raw title".into());
        entry.terminal_title_stripped = Some("raw title".into());
        entry
            .tokens
            .insert("terminal_title".into(), "custom title".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]],
            ..Default::default()
        };

        assert_eq!(
            agent_rows(&config, context(&entry), "working"),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("⠋ raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::TerminalTitle("raw title".into())),
                ResolvedToken::unstyled(ResolvedTokenKind::Custom("custom title".into())),
            ]]
        );
    }

    #[test]
    fn known_agent_override_replaces_default_rows() {
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Workspace]],
            ..Default::default()
        };
        config
            .rows_by_agent
            .insert("pi".into(), vec![vec![AgentSidebarToken::Agent]]);
        let mut pi = entry();
        pi.agent_label = Some("renamed pi".into());

        assert_eq!(
            agent_rows(&config, context(&pi), "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Agent(
                "renamed pi".into()
            ))]]
        );

        pi.canonical_agent = None;
        assert_eq!(
            agent_rows(&config, context(&pi), "working"),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Workspace(
                "repo".into()
            ))]]
        );
    }

    #[test]
    fn grouped_children_suppress_all_builtin_git_details() {
        let config = SpacesSidebarConfig::default();

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "idle",
                    ahead_behind: Some((2, 1)),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                },
            ),
            vec![vec![
                ResolvedToken::unstyled(ResolvedTokenKind::StateIcon),
                ResolvedToken::unstyled(ResolvedTokenKind::Workspace("feature".into())),
            ]]
        );
    }

    #[test]
    fn workspace_custom_token_can_replace_git_specific_details() {
        let tokens = std::collections::HashMap::from([("jj_status".into(), "2 changes".into())]);
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Custom("jj_status".into())]],
            ..Default::default()
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "repo",
                    branch: None,
                    state_text: "idle",
                    ahead_behind: None,
                    tokens: &tokens,
                    suppress_git_details: false,
                },
            ),
            vec![vec![ResolvedToken::unstyled(ResolvedTokenKind::Custom(
                "2 changes".into()
            ))]]
        );
    }

    fn activity_texts(parts: &[ActivityPart]) -> Vec<&str> {
        parts.iter().map(|part| part.text.as_str()).collect()
    }

    #[test]
    fn background_parts_use_claude_glyphs() {
        let tokens = std::collections::HashMap::from([
            ("oasis:shells".to_owned(), "1".to_owned()),
            ("oasis:monitors".to_owned(), "2".to_owned()),
            ("oasis:agents".to_owned(), "4".to_owned()),
        ]);
        let parts = background_parts(&tokens);
        assert_eq!(activity_texts(&parts), ["$1", "∿2", "◯◯◯◯"]);
        assert_eq!(parts[0].color, COUNT_COLOR);
        assert_eq!(parts[2].color, SUBAGENT_COLOR);
        assert!(background_parts(&std::collections::HashMap::new()).is_empty());
    }

    #[test]
    fn background_token_draws_each_part_in_its_own_color() {
        use ratatui::style::Style;
        let config: AgentsSidebarConfig = toml::from_str(r#"rows = [["background"]]"#).unwrap();
        let mut entry = entry();
        entry.tokens = std::collections::HashMap::from([
            ("oasis:shells".to_owned(), "3".to_owned()),
            ("oasis:agents".to_owned(), "2".to_owned()),
        ]);
        let rows = agent_rows(&config, context(&entry), "working");
        let draw = |width| {
            super::super::resolved_token_spans(
                &rows[0],
                ("*", Style::default()),
                Style::default(),
                Style::default(),
                Style::default(),
                Style::default(),
                &super::super::Palette::catppuccin(),
                width,
            )
        };
        let spans = draw(40);
        let drawn = spans
            .iter()
            .map(|span| (span.content.as_ref(), span.style.fg))
            .collect::<Vec<_>>();
        assert_eq!(
            drawn,
            [
                ("$3", Some(COUNT_COLOR.ratatui())),
                (" ◯◯", Some(SUBAGENT_COLOR.ratatui()))
            ]
        );
        let narrow = draw(2);
        let width = narrow
            .iter()
            .map(|span| super::super::display_width(&span.content))
            .sum::<usize>();
        assert!(width <= 2);
    }

    #[test]
    fn loop_part_carries_kind_and_label_for_front_ends() {
        let part = |key: &str, value: &str| {
            loop_part(&std::collections::HashMap::from([(
                key.to_owned(),
                value.to_owned(),
            )]))
            .unwrap()
        };
        let timed = part("oasis:loop", "22:42");
        assert_eq!(timed.kind, ActivityKind::Loop);
        assert_eq!(
            (timed.text.as_str(), timed.label.as_deref()),
            ("↻ 22:42", Some("22:42"))
        );
        let running = part("oasis:loop", "");
        assert_eq!((running.text.as_str(), running.label), ("↻", None));
        // Older servers only send the next wakeup.
        assert_eq!(part("oasis:loop_next", "19:10").text, "↻ 19:10");
    }

    #[test]
    fn goal_and_loop_hide_shells_and_monitors_and_leave_their_mark_to_the_state_icon() {
        let mut tokens = std::collections::HashMap::from([
            ("oasis:shells".to_owned(), "3".to_owned()),
            ("oasis:monitors".to_owned(), "1".to_owned()),
            ("oasis:agents".to_owned(), "2".to_owned()),
            ("oasis:loop".to_owned(), "22:42".to_owned()),
        ]);
        assert_eq!(activity_texts(&background_parts(&tokens)), ["◯◯"]);
        assert_eq!(AgentMode::from_tokens(&tokens), Some(AgentMode::Loop));
        tokens.insert("oasis:goal".to_owned(), "1".to_owned());
        assert_eq!(AgentMode::from_tokens(&tokens), Some(AgentMode::Goal));
        tokens.remove("oasis:loop");
        assert_eq!(activity_texts(&background_parts(&tokens)), ["◯◯"]);
        tokens.remove("oasis:goal");
        assert_eq!(AgentMode::from_tokens(&tokens), None);
        assert_eq!(
            activity_texts(&background_parts(&tokens)),
            ["$3", "∿1", "◯◯"]
        );
    }
}

#[cfg(test)]
mod agent_icon_tests {
    use super::*;

    #[test]
    fn agent_icon_marks_known_agents_and_falls_back_to_a_letter() {
        let claude = agent_icon_part(Some(crate::detect::Agent::Claude), Some("claude")).unwrap();
        assert_eq!(
            (claude.text.as_str(), claude.kind),
            ("✳", ActivityKind::Agent)
        );
        assert_eq!(claude.color, CLAUDE_COLOR);
        let pi = agent_icon_part(Some(crate::detect::Agent::Pi), Some("pi")).unwrap();
        assert_eq!(pi.text, "π");
        // Agents without a mark get a colored letter that is stable per name.
        let amp = agent_icon_part(Some(crate::detect::Agent::Amp), Some("amp")).unwrap();
        assert_eq!(amp.text, "A");
        assert!(AGENT_LETTER_COLORS.contains(&amp.color));
        assert_eq!(
            agent_icon_part(Some(crate::detect::Agent::Amp), Some("amp")).unwrap(),
            amp
        );
        assert!(agent_icon_part(None, None).is_none());
    }

    #[test]
    fn agent_icon_token_parses_from_config() {
        let config: crate::config::Config = toml::from_str(
            "[ui.sidebar.agents.rows_by_agent]\nclaude = [[\"state_icon\", \"agent_icon\", \"workspace\"]]\n",
        )
        .unwrap();
        let rows = config
            .ui
            .sidebar
            .agents
            .rows_for_agent(Some(crate::detect::Agent::Claude));
        assert!(rows[0].contains(&crate::config::AgentSidebarToken::AgentIcon));
    }
}
