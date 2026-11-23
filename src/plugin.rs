//! Route Zellij events to rendering, widget polling, and tool pane actions.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use zellij_tile::prelude::{actions::Action, *};

use crate::{
    config::{ButtonAction, Config, Source},
    render::{Hit, Tab, Target, View},
};
#[cfg(test)]
use self::tests::{
    get_focused_pane_info, get_pane_cwd, get_plugin_ids, request_permission, resize_pane_with_id,
    run_action, run_command_with_env_variables_and_cwd, set_selectable, set_timeout, subscribe,
};

/// Zellij plugin state. Each tab gets its own toolbar and tool pane state.
#[derive(Debug, Default)]
pub struct Toolbar {
    config: Config,
    error: Option<String>,
    mode: ModeInfo,
    theme_received: bool,
    ready: bool,
    tabs: Vec<TabInfo>,
    panes: PaneManifest,
    plugin_id: Option<u32>,
    focused: Option<PaneId>,
    commands: HashMap<PaneId, String>,
    tool_panes: HashMap<String, ToolPane>,
    closed_during_open: HashSet<PaneId>,
    widgets: Vec<WidgetState>,
    initial_cwd: PathBuf,
    hits: Vec<Hit>,
    hover: Option<Target>,
}

impl Toolbar {
    fn instance_tab(&self) -> Option<usize> {
        let id = self.plugin_id?;
        self.panes.panes.iter().find_map(|(position, panes)| {
            panes
                .iter()
                .any(|pane| pane.is_plugin && pane.id == id)
                .then_some(*position)
        })
    }

    fn foreground(&self) -> bool {
        self.instance_tab().is_some_and(|position| {
            self.tabs
                .iter()
                .any(|tab| tab.position == position && tab.active)
        })
    }

    fn refresh_widgets(&mut self) -> bool {
        if !self.ready || !self.config.needs_commands() || !self.foreground() {
            return false;
        }
        let focused = get_focused_pane_info().ok().map(|(_, pane)| pane);
        self.focused = focused;
        let cwd = focused.and_then(|pane| get_pane_cwd(pane).ok());
        let now = Instant::now();
        let mut changed = false;
        for (index, (widget, state)) in self
            .config
            .widgets
            .iter()
            .zip(&mut self.widgets)
            .enumerate()
        {
            let Source::Command {
                args,
                every,
                focused,
            } = &widget.source
            else {
                continue;
            };
            let path = if *focused {
                cwd.as_ref()
            } else {
                Some(&self.initial_cwd)
            };
            if state.cwd.as_ref() != path {
                state.cwd = path.cloned();
                changed |= !state.value.is_empty();
                state.value.clear();
                state.due = None;
                state.generation = state.generation.wrapping_add(1);
            }
            let Some(path) = path else { continue };
            if state.running || state.due.is_some_and(|due| now < due) {
                continue;
            }
            state.running = true;
            state.due = now.checked_add(*every);
            let context = BTreeMap::from([
                ("widget".into(), index.to_string()),
                ("generation".into(), state.generation.to_string()),
            ]);
            let args: Vec<_> = args.iter().map(String::as_str).collect();
            run_command_with_env_variables_and_cwd(&args, BTreeMap::new(), path.clone(), context);
        }
        changed
    }

    fn widget_result(
        &mut self,
        code: Option<i32>,
        stdout: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        let Some(index) = context
            .get("widget")
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return false;
        };
        let Some(state) = self.widgets.get_mut(index) else {
            return false;
        };
        state.running = false;
        let generation = context
            .get("generation")
            .and_then(|value| value.parse::<u64>().ok());
        if generation != Some(state.generation) {
            return self.refresh_widgets();
        }
        let value = if code == Some(0) {
            // Rendering also bounds the cell width. Keep large command output out of state.
            let sample = stdout.get(..stdout.len().min(8192)).unwrap_or(stdout);
            let printable = strip_ansi_escapes::strip(sample);
            crate::render::clean(&String::from_utf8_lossy(&printable))
        } else {
            String::new()
        };
        let changed = state.value != value;
        state.value = value;
        changed
    }

    fn activate(&mut self, target: Target) -> bool {
        if !self.ready || !self.foreground() {
            return false;
        }
        match target {
            Target::Tab(id) => {
                let position = self
                    .tabs
                    .iter()
                    .find(|tab| tab.tab_id == id)
                    .and_then(|tab| u32::try_from(tab.position + 1).ok());
                if let Some(index) = position {
                    run_action(Action::GoToTab { index }, BTreeMap::new());
                }
                false
            }
            Target::Button(index) => self.activate_button(index),
        }
    }

    fn activate_button(&mut self, index: usize) -> bool {
        let Some(button) = self.config.buttons.get(index) else {
            return false;
        };
        match &button.action {
            ButtonAction::Run { actions, focused } => {
                let cwd = focused.then(Self::focused_cwd).flatten();
                for mut action in actions.iter().cloned() {
                    if let Some(cwd) = &cwd {
                        Self::set_run_cwd(&mut action, cwd);
                    }
                    run_action(action, BTreeMap::new());
                }
                false
            }
            ButtonAction::Toggle { open, .. } => {
                match self.tool_panes.get_mut(&button.id) {
                    None => {
                        self.tool_panes.insert(button.id.clone(), ToolPane::Opening);
                        let mut action = (**open).clone();
                        if let Action::NewTiledPane {
                            command: Some(command),
                            ..
                        } = &mut action
                        {
                            command.hold_on_close = false;
                        }
                        if let Some(cwd) = Self::focused_cwd() {
                            Self::set_run_cwd(&mut action, &cwd);
                        }
                        run_action(action, BTreeMap::from([("tool".into(), button.id.clone())]));
                    }
                    Some(state @ ToolPane::Open(_)) => {
                        if let ToolPane::Open(pane_id) = *state {
                            *state = ToolPane::Closing(pane_id);
                            run_action(Action::CloseFocusByPaneId { pane_id }, BTreeMap::new());
                        }
                    }
                    Some(ToolPane::Opening | ToolPane::Closing(_)) => {}
                }
                true
            }
        }
    }

    fn focused_cwd() -> Option<PathBuf> {
        let (_, pane) = get_focused_pane_info().ok()?;
        get_pane_cwd(pane).ok()
    }

    fn set_run_cwd(action: &mut Action, cwd: &Path) {
        if let Action::NewTiledPane {
            command: Some(command),
            ..
        }
        | Action::NewFloatingPane {
            command: Some(command),
            ..
        }
        | Action::NewStackedPane {
            command: Some(command),
            ..
        }
        | Action::NewInPlacePane {
            command: Some(command),
            ..
        } = action
        {
            if command.cwd.is_none() {
                command.cwd = Some(cwd.to_path_buf());
            }
        }
    }

    fn tool_opened(
        &mut self,
        action: Action,
        pane: Option<PaneId>,
        context: &BTreeMap<String, String>,
    ) -> bool {
        let Some(id) = context.get("tool") else {
            return false;
        };
        if self.tool_panes.get(id) != Some(&ToolPane::Opening) {
            return false;
        }
        match pane.filter(|pane| !self.closed_during_open.contains(pane)) {
            Some(pane) => {
                self.tool_panes.insert(id.clone(), ToolPane::Open(pane));
                self.grow_tool(id, &action, pane);
            }
            None => {
                self.tool_panes.remove(id);
            }
        }
        if !self
            .tool_panes
            .values()
            .any(|state| *state == ToolPane::Opening)
        {
            self.closed_during_open.clear();
        }
        true
    }

    fn grow_tool(&self, id: &str, action: &Action, pane: PaneId) {
        let Some(ButtonAction::Toggle { grow, .. }) = self
            .config
            .buttons
            .iter()
            .find(|button| button.id == id)
            .map(|button| &button.action)
        else {
            return;
        };
        let direction = match action {
            Action::NewTiledPane {
                direction: Some(Direction::Down),
                ..
            } => Direction::Up,
            Action::NewTiledPane {
                direction: Some(Direction::Right),
                ..
            } => Direction::Left,
            _ => return,
        };
        let strategy = ResizeStrategy {
            resize: Resize::Increase,
            direction: Some(direction),
            invert_on_boundaries: false,
        };
        for _ in 0..*grow {
            resize_pane_with_id(strategy, pane);
        }
    }

    fn pane_closed(&mut self, pane: PaneId) -> bool {
        self.commands.remove(&pane);
        if self
            .tool_panes
            .values()
            .any(|state| *state == ToolPane::Opening)
        {
            self.closed_during_open.insert(pane);
        }
        let old_count = self.tool_panes.len();
        self.tool_panes
            .retain(|_, state| *state != ToolPane::Open(pane) && *state != ToolPane::Closing(pane));
        old_count != self.tool_panes.len()
    }

    fn target_at(&self, row: isize, column: usize) -> Option<Target> {
        self.hits
            .iter()
            .find(|hit| hit.row == row && hit.columns.contains(&column))
            .map(|hit| hit.target)
    }

    fn mouse(&mut self, mouse: Mouse) -> bool {
        match mouse {
            Mouse::LeftClick(row, column) => self
                .target_at(row, column)
                .is_some_and(|target| self.activate(target)),
            Mouse::Hover(row, column) => {
                let target = self.target_at(row, column);
                let changed = self.hover != target;
                self.hover = target;
                changed
            }
            _ => false,
        }
    }

    fn icon_for(&self, tab: &TabInfo) -> &str {
        let pane = self
            .panes
            .panes
            .get(&tab.position)
            .and_then(|panes| panes.iter().find(|pane| pane.is_focused && !pane.is_plugin));
        let program = pane
            .and_then(|pane| {
                self.commands
                    .get(&PaneId::Terminal(pane.id))
                    .map(String::as_str)
                    .or_else(|| pane.terminal_command.as_deref()?.split_whitespace().next())
            })
            .and_then(|command| Path::new(command).file_name()?.to_str());
        self.config
            .tabs
            .icons
            .iter()
            .find(|icon| {
                icon.program
                    .as_deref()
                    .is_some_and(|name| Some(name) == program)
                    || icon.title.as_deref().is_some_and(|name| name == tab.name)
            })
            .map_or(&self.config.tabs.fallback, |icon| &icon.glyph)
    }
}

impl ZellijPlugin for Toolbar {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.plugin_id = Some(ids.plugin_id);
        self.initial_cwd = ids.initial_cwd;
        match Config::parse(configuration.get("config").map_or("", String::as_str)) {
            Ok(config) => self.config = config,
            Err(error) => {
                set_selectable(false);
                self.error = Some(format!("Toolbar configuration: {error}"));
                return;
            }
        }
        self.widgets
            .resize_with(self.config.widgets.len(), WidgetState::default);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::ModeUpdate,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::PaneClosed,
            EventType::ActionComplete,
            EventType::CommandChanged,
            EventType::Mouse,
            EventType::CwdChanged,
            EventType::Timer,
            EventType::RunCommandResult,
        ]);
        let mut permissions = vec![
            PermissionType::ReadApplicationState,
            PermissionType::RunActionsAsUser,
        ];
        if self.config.needs_commands() {
            permissions.push(PermissionType::RunCommands);
        }
        if self
            .config
            .buttons
            .iter()
            .any(|button| matches!(button.action, ButtonAction::Toggle { grow: 1.., .. }))
        {
            permissions.push(PermissionType::ChangeApplicationState);
        }
        // Keep the pane focusable while Zellij displays its permission prompt.
        request_permission(&permissions);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                set_selectable(false);
                self.ready = true;
                self.refresh_widgets();
                if self.config.needs_commands() {
                    set_timeout(1.0);
                }
                true
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                set_selectable(false);
                self.error = Some("Toolbar permissions were denied".into());
                true
            }
            Event::ModeUpdate(mode) => {
                self.theme_received = true;
                self.mode = mode;
                true
            }
            Event::TabUpdate(tabs) => {
                let changed = self.tabs != tabs;
                self.tabs = tabs;
                if changed {
                    self.refresh_widgets();
                }
                changed
            }
            Event::PaneUpdate(panes) => {
                self.panes = panes;
                self.refresh_widgets();
                true
            }
            Event::CwdChanged(pane, _, _) if Some(pane) == self.focused => {
                self.refresh_widgets();
                true
            }
            Event::CommandChanged(pane, args, _, _) => {
                if let Some(program) = args.into_iter().next() {
                    self.commands.insert(pane, program);
                } else {
                    self.commands.remove(&pane);
                }
                true
            }
            Event::Mouse(mouse) | Event::MouseWithModifiers(mouse, _) => self.mouse(mouse),
            Event::Timer(_) => {
                let changed = self.refresh_widgets();
                set_timeout(1.0);
                changed
            }
            Event::RunCommandResult(code, stdout, _, context) => {
                self.widget_result(code, &stdout, &context)
            }
            Event::ActionComplete(action, pane, context) => {
                self.tool_opened(action, pane, &context)
            }
            Event::PaneClosed(pane) => self.pane_closed(pane),
            _ => false,
        }
    }

    fn pipe(&mut self, pipe: PipeMessage) -> bool {
        if pipe.name != "toolbar" || pipe.source != PipeSource::Keybind {
            return false;
        }
        let Some(id) = pipe.payload else { return false };
        let index = self
            .config
            .buttons
            .iter()
            .position(|button| button.id == id);
        index.is_some_and(|index| self.activate(Target::Button(index)))
    }

    fn render(&mut self, rows: usize, columns: usize) {
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| Tab {
                id: tab.tab_id,
                number: tab.position + 1,
                name: &tab.name,
                icon: self.icon_for(tab),
                active: tab.active,
            })
            .collect();
        let values: Vec<_> = self
            .config
            .widgets
            .iter()
            .zip(&self.widgets)
            .map(|(widget, state)| match &widget.source {
                Source::Session => self.mode.session_name.clone().unwrap_or_default(),
                Source::Text(text) => text.clone(),
                Source::Command { .. } => state.value.clone(),
            })
            .collect();
        let open: Vec<_> = self
            .config
            .buttons
            .iter()
            .map(|button| {
                matches!(
                    self.tool_panes.get(&button.id),
                    Some(ToolPane::Open(_) | ToolPane::Closing(_))
                )
            })
            .collect();
        let view = View {
            theme: self.theme_received.then_some(self.mode.style.colors),
            tabs: &tabs,
            tab_options: &self.config.tabs,
            widgets: &self.config.widgets,
            values: &values,
            buttons: &self.config.buttons,
            open: &open,
            hover: self.hover,
            error: self.error.as_deref(),
        };
        let (rendered, hits) = view.render(rows, columns);
        self.hits = hits;
        print!("{rendered}");
        if let Err(error) = io::stdout().flush() {
            eprintln!("Unable to render the toolbar: {error}");
        }
    }
}

#[derive(Debug, Default)]
struct WidgetState {
    cwd: Option<PathBuf>,
    due: Option<Instant>,
    running: bool,
    generation: u64,
    value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolPane {
    Opening,
    Open(PaneId),
    Closing(PaneId),
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    thread_local! {
        static HOST: RefCell<TestHost> = RefCell::new(TestHost::default());
    }

    #[derive(Default)]
    struct TestHost {
        actions: Vec<(Action, BTreeMap<String, String>)>,
        commands: Vec<(PathBuf, BTreeMap<String, String>)>,
        resize: Vec<PaneId>,
        selectable: Vec<bool>,
        cwd: PathBuf,
    }

    #[test]
    fn keeps_the_toolbar_focusable_until_permissions_are_answered() {
        for status in [PermissionStatus::Granted, PermissionStatus::Denied] {
            let mut toolbar = load();
            HOST.with_borrow(|host| assert!(host.selectable.is_empty()));
            assert!(toolbar.update(Event::PermissionRequestResult(status)));
            HOST.with_borrow(|host| assert_eq!(host.selectable, vec![false]));
        }
    }

    #[test]
    fn routes_clicks_and_shortcuts_to_the_same_tool_in_the_active_tab() {
        let mut toolbar = load();
        let tab = TabInfo {
            tab_id: 6,
            active: true,
            ..TabInfo::default()
        };
        assert!(toolbar.update(Event::TabUpdate(vec![tab])));
        assert!(toolbar.update(Event::PaneUpdate(panes())));
        assert!(toolbar.update(Event::PermissionRequestResult(PermissionStatus::Granted)));

        toolbar.hits = vec![
            Hit {
                row: 1,
                columns: 2..8,
                target: Target::Button(0),
            },
            Hit {
                row: 0,
                columns: 2..8,
                target: Target::Tab(6),
            },
        ];
        toolbar.update(Event::Mouse(Mouse::LeftClick(1, 4)));
        toolbar.update(Event::Mouse(Mouse::Release(1, 4)));
        toolbar.pipe(shortcut("tool"));
        let requests = HOST.with_borrow_mut(|host| std::mem::take(&mut host.actions));
        let [(open, context)] = requests.as_slice() else {
            panic!("Only the left press should create a pane");
        };
        assert!(
            matches!(open, Action::NewTiledPane { command: Some(command), .. }
            if command.cwd.as_deref() == Some(Path::new("/first")) && !command.hold_on_close)
        );
        let pane = PaneId::Terminal(12);
        assert!(toolbar.update(Event::ActionComplete(
            open.clone(),
            Some(pane),
            context.clone()
        )));
        assert_eq!(toolbar.tool_panes.get("tool"), Some(&ToolPane::Open(pane)));
        HOST.with_borrow(|host| assert_eq!(host.resize, vec![pane, pane]));

        toolbar.pipe(shortcut("tool"));
        toolbar.pipe(shortcut("tool"));
        HOST.with_borrow(|host| {
            assert_eq!(
                host.actions,
                vec![(
                    Action::CloseFocusByPaneId { pane_id: pane },
                    BTreeMap::new()
                )]
            );
        });
        assert!(toolbar.update(Event::PaneClosed(pane)));
        assert!(!toolbar.tool_panes.contains_key("tool"));
        toolbar.update(Event::Mouse(Mouse::LeftClick(0, 4)));
        toolbar.pipe(shortcut("command"));
        HOST.with_borrow(|host| {
            assert!(matches!(
                host.actions.get(1),
                Some((Action::GoToTab { index: 1 }, _))
            ));
            assert!(matches!(host.actions.get(2), Some((Action::NewTiledPane {
                command: Some(command), ..
            }, _)) if command.cwd.as_deref() == Some(Path::new("/first"))));
        });
    }

    #[test]
    fn keeps_background_widget_output_bound_to_its_focused_directory() {
        let mut toolbar = load();
        let terminal = PaneId::Terminal(2);
        let tab = TabInfo {
            tab_id: 6,
            active: true,
            ..TabInfo::default()
        };
        toolbar.update(Event::ModeUpdate(ModeInfo::default()));
        toolbar.update(Event::TabUpdate(vec![tab]));
        toolbar.update(Event::PaneUpdate(panes()));
        toolbar.update(Event::PermissionRequestResult(PermissionStatus::Granted));
        toolbar.update(Event::Timer(1.0));
        let first = HOST.with_borrow(|host| {
            let [(cwd, context)] = host.commands.as_slice() else {
                panic!("A timer must not start a concurrent command");
            };
            assert_eq!(cwd, Path::new("/first"));
            context.clone()
        });
        HOST.with_borrow_mut(|host| host.cwd = PathBuf::from("/second"));
        toolbar.update(Event::CwdChanged(
            terminal,
            PathBuf::from("/second"),
            vec![1],
        ));
        toolbar.update(Event::RunCommandResult(
            Some(0),
            b"old".to_vec(),
            vec![],
            first,
        ));
        assert!(toolbar
            .widgets
            .first()
            .expect("command widget")
            .value
            .is_empty());
        let context = HOST.with_borrow(|host| host.commands.last().expect("second run").1.clone());
        assert!(toolbar.update(Event::RunCommandResult(
            Some(0),
            b"\x1b[31mnew\x1b[0m".to_vec(),
            vec![],
            context,
        )));
        assert_eq!(
            toolbar.widgets.first().expect("command widget").value,
            "new"
        );
        toolbar.update(Event::CommandChanged(
            terminal,
            vec!["tool".into()],
            true,
            vec![1],
        ));
        assert_eq!(
            toolbar.commands.get(&terminal).map(String::as_str),
            Some("tool")
        );
        assert!(!toolbar.update(Event::Visible(true)));
    }

    fn load() -> Toolbar {
        HOST.with_borrow_mut(|host| {
            *host = TestHost {
                cwd: "/first".into(),
                ..TestHost::default()
            }
        });
        let mut toolbar = Toolbar::default();
        toolbar.load(BTreeMap::from([(
            "config".into(),
            r#"
                widget { command "git" "branch" cwd="focused"; }
                widget { session; }
                button "tool" { toggle grow=2 { Run "tool" { direction "Down"; }; }; }
                button "command" { action cwd="focused" { Run "tool"; }; }
            "#
            .into(),
        )]));
        assert!(toolbar.error.is_none());
        toolbar
    }

    fn panes() -> PaneManifest {
        PaneManifest {
            panes: HashMap::from([(
                0,
                vec![PaneInfo {
                    id: 1,
                    is_plugin: true,
                    ..PaneInfo::default()
                }],
            )]),
        }
    }

    fn shortcut(id: &str) -> PipeMessage {
        PipeMessage::new(
            PipeSource::Keybind,
            "toolbar",
            &Some(id.into()),
            &None,
            true,
        )
    }

    pub(super) fn get_focused_pane_info() -> Result<(usize, PaneId), String> {
        Ok((0, PaneId::Terminal(2)))
    }

    pub(super) fn get_pane_cwd(_: PaneId) -> Result<PathBuf, String> {
        Ok(HOST.with_borrow(|host| host.cwd.clone()))
    }

    pub(super) fn get_plugin_ids() -> PluginIds {
        PluginIds {
            plugin_id: 1,
            client_id: 1,
            zellij_pid: 1,
            initial_cwd: "/session".into(),
        }
    }

    pub(super) fn request_permission(_: &[PermissionType]) {}
    pub(super) fn set_selectable(selectable: bool) {
        HOST.with_borrow_mut(|host| host.selectable.push(selectable));
    }
    pub(super) fn set_timeout(_: f64) {}
    pub(super) fn subscribe(_: &[EventType]) {}

    pub(super) fn resize_pane_with_id(_: ResizeStrategy, pane: PaneId) {
        HOST.with_borrow_mut(|host| host.resize.push(pane));
    }

    pub(super) fn run_action(action: Action, context: BTreeMap<String, String>) {
        HOST.with_borrow_mut(|host| host.actions.push((action, context)));
    }

    pub(super) fn run_command_with_env_variables_and_cwd(
        _: &[&str],
        _: BTreeMap<String, String>,
        cwd: PathBuf,
        context: BTreeMap<String, String>,
    ) {
        HOST.with_borrow_mut(|host| host.commands.push((cwd, context)));
    }
}
