//! Parse the layout's embedded KDL and reuse Zellij's native action grammar.

use std::{collections::BTreeSet, time::Duration};

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::{actions::Action, Direction};
use zellij_utils::{
    input::{keybinds::Keybinds, options::Options},
    plugin_api::action::ProtobufAction,
};

#[derive(Debug, Default)]
pub(crate) struct Config {
    pub(crate) tabs: Tabs,
    pub(crate) widgets: Vec<Widget>,
    pub(crate) buttons: Vec<Button>,
}

impl Config {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let document = source
            .parse::<KdlDocument>()
            .map_err(|error| error.to_string())?;
        let mut config = Self::default();
        let mut ids = BTreeSet::new();
        let mut has_tabs = false;
        for node in document.nodes() {
            match node.name().value() {
                "tabs" if !has_tabs => {
                    config.tabs = Tabs::parse(node)?;
                    has_tabs = true;
                }
                "widget" => config.widgets.push(Widget::parse(node)?),
                "button" => {
                    let button = Button::parse(node)?;
                    if !ids.insert(button.id.clone()) {
                        return Err(format!("Duplicate button ID: {}", button.id));
                    }
                    config.buttons.push(button);
                }
                name => return Err(format!("Unexpected toolbar node: {name}")),
            }
        }
        Ok(config)
    }

    pub(crate) fn needs_commands(&self) -> bool {
        self.widgets
            .iter()
            .any(|widget| matches!(widget.source, Source::Command { .. }))
    }
}

#[derive(Debug)]
pub(crate) struct Tabs {
    pub(crate) numbers: bool,
    pub(crate) active_accent: Option<usize>,
    pub(crate) icons: Vec<Icon>,
    pub(crate) fallback: String,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            numbers: true,
            active_accent: None,
            icons: Vec::new(),
            fallback: String::new(),
        }
    }
}

impl Tabs {
    fn parse(node: &KdlNode) -> Result<Self, String> {
        let mut tabs = Self {
            numbers: node
                .get("numbers")
                .and_then(|entry| entry.value().as_bool())
                .unwrap_or(true),
            fallback: string_property(node, "icon").unwrap_or_default(),
            ..Self::default()
        };
        tabs.active_accent = node
            .get("active-accent")
            .map(|_| bounded(node, "active-accent", 0, 0, 3))
            .transpose()?;
        for icon in children(node) {
            if icon.name().value() != "icon" {
                return Err(format!("Unexpected tabs node: {}", icon.name().value()));
            }
            let glyph = string_argument(icon)?;
            let program = string_property(icon, "program");
            let title = string_property(icon, "title");
            if program.is_some() == title.is_some() {
                return Err("A tab icon needs exactly one of program or title".into());
            }
            tabs.icons.push(Icon {
                glyph,
                program,
                title,
            });
        }
        Ok(tabs)
    }
}

#[derive(Debug)]
pub(crate) struct Icon {
    pub(crate) glyph: String,
    pub(crate) program: Option<String>,
    pub(crate) title: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Widget {
    pub(crate) label: String,
    pub(crate) priority: i64,
    pub(crate) gap: usize,
    pub(crate) max_width: usize,
    pub(crate) source: Source,
}

impl Widget {
    fn parse(node: &KdlNode) -> Result<Self, String> {
        let label = string_property(node, "label").unwrap_or_default();
        let priority = integer_property(node, "priority").unwrap_or(0);
        let gap = bounded(node, "gap", 2, 0, 16)?;
        let max_width = bounded(node, "max-width", 32, 1, 256)?;
        let [source] = children(node) else {
            return Err("A widget needs exactly one source".into());
        };
        let source = Source::parse(source)?;
        Ok(Self {
            label,
            priority,
            gap,
            max_width,
            source,
        })
    }
}

#[derive(Debug)]
pub(crate) enum Source {
    Session,
    Text(String),
    Command {
        args: Vec<String>,
        every: Duration,
        focused: bool,
    },
}

impl Source {
    fn parse(source: &KdlNode) -> Result<Self, String> {
        match source.name().value() {
            "session" => Ok(Self::Session),
            "text" => Ok(Self::Text(string_argument(source)?)),
            "command" => Self::command(source),
            name => Err(format!("Unknown widget source: {name}")),
        }
    }

    fn command(source: &KdlNode) -> Result<Self, String> {
        let args: Result<Vec<_>, _> = source
            .entries()
            .iter()
            .filter(|entry| entry.name().is_none())
            .map(|entry| {
                entry
                    .value()
                    .as_string()
                    .map(str::to_owned)
                    .ok_or("Command arguments must be strings")
            })
            .collect();
        let args = args?;
        if args.first().is_none_or(String::is_empty) {
            return Err("A command widget needs an executable".into());
        }
        let every = bounded(source, "every", 5, 1, 86_400)?;
        let focused = match string_property(source, "cwd").as_deref() {
            None | Some("session") => false,
            Some("focused") => true,
            Some(value) => return Err(format!("Unknown widget cwd: {value}")),
        };
        Ok(Self::Command {
            args,
            every: Duration::from_secs(every as u64),
            focused,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Button {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) icon: String,
    pub(crate) on_icon: Option<String>,
    pub(crate) shortcut: String,
    pub(crate) accent: usize,
    pub(crate) action: ButtonAction,
}

impl Button {
    fn parse(node: &KdlNode) -> Result<Self, String> {
        let id = string_argument(node)?;
        if id.is_empty() || id.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err("A button ID must be nonempty and contain no whitespace".into());
        }
        let label = string_property(node, "label").unwrap_or_else(|| id.clone());
        let icon = string_property(node, "icon").unwrap_or_default();
        if label.trim().is_empty() && icon.trim().is_empty() {
            return Err(format!("Button {id} needs a visible label or icon"));
        }
        let [behavior] = children(node) else {
            return Err(format!(
                "Button {id} needs exactly one action or toggle block"
            ));
        };
        let action =
            ButtonAction::parse(behavior).map_err(|error| format!("Button {id}: {error}"))?;
        Ok(Self {
            id,
            label,
            icon,
            action,
            on_icon: string_property(node, "on-icon"),
            shortcut: string_property(node, "shortcut").unwrap_or_default(),
            accent: bounded(node, "accent", 0, 0, 3)?,
        })
    }
}

#[derive(Debug)]
pub(crate) enum ButtonAction {
    Run { actions: Vec<Action>, focused: bool },
    Toggle { open: Box<Action>, grow: usize },
}

impl ButtonAction {
    fn parse(behavior: &KdlNode) -> Result<Self, String> {
        let actions = native_actions(behavior)?;
        match behavior.name().value() {
            "action" => Ok(Self::Run {
                actions,
                focused: match string_property(behavior, "cwd").as_deref() {
                    None => false,
                    Some("focused") => true,
                    Some(value) => return Err(format!("Unknown action cwd: {value}")),
                },
            }),
            "toggle" => {
                let [action] = <[Action; 1]>::try_from(actions).map_err(|actions| {
                    format!("A toggle needs one Run action, got {}", actions.len())
                })?;
                if !matches!(
                    action,
                    Action::NewTiledPane {
                        direction: Some(Direction::Down | Direction::Right),
                        command: Some(_),
                        ..
                    }
                ) {
                    return Err("Toggle needs one Run with Down or Right".into());
                }
                let grow = bounded(behavior, "grow", 0, 0, 12)?;
                Ok(Self::Toggle {
                    open: Box::new(action),
                    grow,
                })
            }
            name => Err(format!("Unknown button behavior: {name}")),
        }
    }
}

// Zellij exposes its action parser as part of the keybinding parser. Use it on
// a private, synthetic binding so any supported native action keeps its grammar.
fn native_actions(node: &KdlNode) -> Result<Vec<Action>, String> {
    let block = node
        .children()
        .ok_or("Expected a block of Zellij actions")?;
    let config = format!("keybinds {{ normal {{ bind \"F12\" {{\n{block}\n}}; }}; }}");
    let mut keybinds = Keybinds::from_string(config, Keybinds::default(), &Options::default())
        .map_err(|error| error.to_string())?;
    let actions = keybinds
        .0
        .remove(&zellij_tile::prelude::InputMode::Normal)
        .and_then(|bindings| bindings.into_values().next())
        .filter(|actions| !actions.is_empty())
        .ok_or("Expected at least one Zellij action")?;
    for action in &actions {
        ProtobufAction::try_from(action.clone())
            .map_err(|error| format!("{action} is unavailable to plugins: {error}"))?;
    }
    Ok(actions)
}

fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map_or(&[], KdlDocument::nodes)
}

fn string_argument(node: &KdlNode) -> Result<String, String> {
    node.get(0)
        .and_then(|entry| entry.value().as_string())
        .map(str::to_owned)
        .ok_or_else(|| format!("{} needs a string argument", node.name().value()))
}

fn string_property(node: &KdlNode, name: &str) -> Option<String> {
    node.get(name)
        .and_then(|entry| entry.value().as_string())
        .map(str::to_owned)
}

fn integer_property(node: &KdlNode, name: &str) -> Option<i64> {
    node.get(name).and_then(|entry| entry.value().as_i64())
}

fn bounded(
    node: &KdlNode,
    property: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, String> {
    if node.get(property).is_none() {
        return Ok(default);
    }
    integer_property(node, property)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (min..=max).contains(value))
        .ok_or_else(|| format!("{property} must be between {min} and {max}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_actions_and_named_toggles() {
        let config = Config::parse(
            r#"
            button "one" { action { ToggleSessionCard; }; }
            button "two" { toggle grow=2 { Run "tool" { direction "Down"; }; }; }
        "#,
        )
        .expect("valid configuration");
        assert!(matches!(
            config.buttons.first().map(|b| &b.action),
            Some(ButtonAction::Run { actions, .. }) if actions == &[Action::ToggleSessionCard]
        ));
        assert!(matches!(
            config.buttons.get(1).map(|b| &b.action),
            Some(ButtonAction::Toggle { grow: 2, .. })
        ));
    }

    #[test]
    fn rejects_ambiguous_or_non_tiled_toggle() {
        Config::parse(r#"button "x" { toggle { Run "tool"; }; }"#)
            .expect_err("toggle needs a direction");
        let duplicate = r#"button "x" { action { ToggleSessionCard; }; }"#;
        Config::parse(&format!("{duplicate}\n{duplicate}")).expect_err("duplicate button ID");
    }
    #[test]
    fn example_has_portable_actions() {
        let document = include_str!("../examples/config.kdl")
            .parse::<KdlDocument>()
            .expect("example plugin alias");
        let source = document
            .get("plugins")
            .and_then(KdlNode::children)
            .and_then(|plugins| plugins.get("toolbar"))
            .and_then(KdlNode::children)
            .and_then(|toolbar| toolbar.get("config"))
            .and_then(|entry| entry.get(0))
            .and_then(|entry| entry.value().as_string())
            .expect("embedded sample config");
        let example = Config::parse(source).expect("supported actions in sample");
        assert_eq!(example.buttons.len(), 6);
        assert_eq!(example.widgets.len(), 7);
    }
}
