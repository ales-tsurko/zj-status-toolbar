//! Terminal-cell layout and hit targets for the two rows.

use std::{fmt::Write, ops::Range};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::{Palette, PaletteColor, Styling};

use crate::config::{Button, Tabs, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Tab(usize),
    Button(usize),
}

#[derive(Debug)]
pub(crate) struct Hit {
    pub(crate) row: isize,
    pub(crate) columns: Range<usize>,
    pub(crate) target: Target,
}

#[derive(Debug)]
pub(crate) struct Tab<'a> {
    pub(crate) id: usize,
    pub(crate) number: usize,
    pub(crate) name: &'a str,
    pub(crate) icon: &'a str,
    pub(crate) active: bool,
}

#[derive(Debug)]
pub(crate) struct View<'a> {
    pub(crate) theme: Option<Styling>,
    pub(crate) tabs: &'a [Tab<'a>],
    pub(crate) tab_options: &'a Tabs,
    pub(crate) widgets: &'a [Widget],
    pub(crate) values: &'a [String],
    pub(crate) buttons: &'a [Button],
    pub(crate) open: &'a [bool],
    pub(crate) hover: Option<Target>,
    pub(crate) error: Option<&'a str>,
}

impl View<'_> {
    pub(crate) fn render(&self, rows: usize, columns: usize) -> (String, Vec<Hit>) {
        let colors = Colors::new(self.theme, self.tab_options.active_accent);
        let mut status = Row::new(columns, colors.foreground, colors.background);
        let mut toolbar = Row::new(columns, colors.foreground, colors.toolbar);
        if let Some(error) = self.error {
            status.push(&clip(&clean(error), columns), colors.foreground, "", None);
        } else {
            self.status(&mut status, &colors);
            if rows >= 2 {
                self.toolbar(&mut toolbar, &colors);
            }
        }
        let (mut output, mut hits) = status.finish(0);
        if rows >= 2 {
            let (bottom, bottom_hits) = toolbar.finish(1);
            output.push_str("\r\n");
            output.push_str(&bottom);
            hits.extend(bottom_hits);
        }
        (output, hits)
    }

    fn status(&self, row: &mut Row, colors: &Colors) {
        let tab_reserve = if self.tabs.is_empty() {
            0
        } else {
            row.columns.min(24)
        };
        let mut widgets: Vec<_> = self
            .widgets
            .iter()
            .zip(self.values)
            .filter_map(|(widget, value)| {
                let value = clean(value);
                if value.is_empty() {
                    return None;
                }
                let text = clip(&join(&clean(&widget.label), &value), widget.max_width);
                Some((widget, text))
            })
            .collect();
        while widget_width(&widgets) + tab_reserve > row.columns {
            let Some((index, _)) = widgets
                .iter()
                .enumerate()
                .min_by_key(|(index, (widget, _))| (widget.priority, *index))
            else {
                break;
            };
            widgets.remove(index);
        }
        let right_width = widget_width(&widgets);
        let tab_limit = row.columns.saturating_sub(right_width);
        self.tab_strip(row, colors, tab_limit);
        row.space(row.columns.saturating_sub(row.column + right_width));
        for (widget, value) in widgets {
            row.space(widget.gap);
            row.push(&value, colors.foreground, "", None);
        }
    }

    fn tab_strip(&self, row: &mut Row, colors: &Colors, limit: usize) {
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| {
                let badge = if self.tab_options.numbers {
                    format!(" {} ", tab.number)
                } else {
                    String::new()
                };
                let name = join(&clean(tab.icon), &clean(tab.name));
                (tab, badge, name)
            })
            .collect();
        let all_width: usize = tabs
            .iter()
            .map(|(_, badge, name)| width(badge) + width(name) + 3)
            .sum();
        if all_width <= limit {
            for (tab, badge, name) in tabs {
                Self::tab(row, colors, tab, &badge, &name, self.hover);
            }
            return;
        }

        let Some(active) = tabs
            .iter()
            .position(|(tab, _, _)| tab.active)
            .or_else(|| (!tabs.is_empty()).then_some(0))
        else {
            return;
        };
        let Some((tab, badge, name)) = tabs.get(active) else {
            return;
        };
        let available = limit.saturating_sub(5);
        let active_width = width(badge) + width(name) + 3;
        if active_width > limit.saturating_sub(2) {
            let name = clip(name, available.saturating_sub(width(badge)));
            Self::tab(row, colors, tab, badge, &name, self.hover);
            if row.column + 2 <= limit {
                row.push(" …", colors.foreground, "", None);
            }
            return;
        }
        let mut remaining = limit.saturating_sub(active_width + 2);
        let mut start = active;
        let mut end = active + 1;
        while let Some((_, badge, name)) = tabs.get(end) {
            let cells = width(badge) + width(name) + 3;
            if cells > remaining {
                break;
            }
            remaining -= cells;
            end += 1;
        }
        while let Some((_, badge, name)) = start.checked_sub(1).and_then(|i| tabs.get(i)) {
            let cells = width(badge) + width(name) + 3;
            if cells > remaining {
                break;
            }
            remaining -= cells;
            start -= 1;
        }
        if start > 0 && end == tabs.len() {
            row.push("… ", colors.foreground, "", None);
        }
        for (tab, badge, name) in tabs.iter().skip(start).take(end - start) {
            Self::tab(row, colors, tab, badge, name, self.hover);
        }
        if end < tabs.len() {
            row.push(" …", colors.foreground, "", None);
        }
    }

    fn tab(
        row: &mut Row,
        colors: &Colors,
        tab: &Tab<'_>,
        badge: &str,
        name: &str,
        hover: Option<Target>,
    ) {
        let foreground = if tab.active {
            colors.active
        } else {
            colors.foreground
        };
        let target = Target::Tab(tab.id);
        let start = row.column;
        row.space(1);
        row.push(badge, foreground, "\x1b[7m", None);
        row.space(1);
        let style = if hover == Some(target) { "\x1b[4m" } else { "" };
        row.push(name, foreground, style, None);
        row.space(1);
        row.hit(start, target);
    }

    fn toolbar(&self, row: &mut Row, colors: &Colors) {
        let buttons: Vec<_> = self
            .buttons
            .iter()
            .zip(self.open)
            .map(|(button, &open)| {
                let icon = if open {
                    button.on_icon.as_deref().unwrap_or(&button.icon)
                } else {
                    &button.icon
                };
                let label = join(&clean(icon), &clean(&button.label));
                let shortcut = clean(&button.shortcut);
                let full = if shortcut.is_empty() {
                    label.clone()
                } else {
                    format!("{label} ({shortcut})")
                };
                (button, open, label, full)
            })
            .collect();
        let wide_width = buttons
            .iter()
            .map(|(_, _, _, full)| width(full) + 2)
            .sum::<usize>();
        let spacing = if wide_width.saturating_sub(1) <= row.columns {
            2
        } else {
            1
        };
        for (index, (button, open, label, full)) in buttons.into_iter().enumerate() {
            let gap = if index == 0 { 1 } else { spacing };
            let overflow = usize::from(index + 1 < self.buttons.len()) * 2;
            let remaining = row.columns.saturating_sub(row.column + gap + overflow);
            let value = if width(&full) <= remaining {
                &full
            } else {
                &label
            };
            if width(value) > remaining {
                if row.column < row.columns {
                    row.space(usize::from(row.column + 1 < row.columns));
                    row.push("…", colors.foreground, "", None);
                }
                break;
            }
            row.space(gap);
            let target = Target::Button(index);
            let style = match (open, self.hover == Some(target)) {
                (true, true) => "\x1b[1;4m",
                (true, false) => "\x1b[1m",
                (false, true) => "\x1b[4m",
                (false, false) => "",
            };
            let accent = colors.accents.get(button.accent).copied().flatten();
            row.push(value, accent.or(colors.foreground), style, Some(target));
        }
    }
}

#[derive(Debug)]
struct Colors {
    foreground: Option<PaletteColor>,
    background: Option<PaletteColor>,
    toolbar: Option<PaletteColor>,
    active: Option<PaletteColor>,
    accents: [Option<PaletteColor>; 4],
}

impl Colors {
    fn new(theme: Option<Styling>, active_accent: Option<usize>) -> Self {
        let Some(theme) = theme else {
            return Self {
                foreground: None,
                background: None,
                toolbar: None,
                active: None,
                accents: [None; 4],
            };
        };
        let text = theme.text_unselected;
        let toolbar = match (text.background, text.base) {
            (PaletteColor::Rgb(bg), PaletteColor::Rgb(fg)) => {
                let mix = |bg, fg| ((u16::from(bg) * 15 + u16::from(fg)) / 16) as u8;
                PaletteColor::Rgb((mix(bg.0, fg.0), mix(bg.1, fg.1), mix(bg.2, fg.2)))
            }
            (background, _) => background,
        };
        let accents = [
            text.emphasis_0,
            text.emphasis_1,
            text.emphasis_2,
            text.emphasis_3,
        ];
        let active = active_accent
            .and_then(|index| accents.get(index))
            .copied()
            .unwrap_or_else(|| Palette::from(theme).blue);
        Self {
            foreground: Some(text.base),
            background: Some(text.background),
            toolbar: Some(toolbar),
            active: Some(readable(active, text.background, text.base)),
            accents: accents.map(|accent| Some(readable(accent, toolbar, text.base))),
        }
    }
}

#[derive(Debug)]
struct Row {
    text: String,
    hits: Vec<(Range<usize>, Target)>,
    column: usize,
    columns: usize,
    foreground: Option<PaletteColor>,
    background: Option<PaletteColor>,
}

impl Row {
    fn new(
        columns: usize,
        foreground: Option<PaletteColor>,
        background: Option<PaletteColor>,
    ) -> Self {
        Self {
            text: String::new(),
            hits: Vec::new(),
            column: 0,
            columns,
            foreground,
            background,
        }
    }

    fn push(
        &mut self,
        text: &str,
        foreground: Option<PaletteColor>,
        style: &str,
        target: Option<Target>,
    ) {
        let text = clip(text, self.columns.saturating_sub(self.column));
        if text.is_empty() {
            return;
        }
        let start = self.column;
        self.column += width(&text);
        self.text.push_str("\x1b[0m");
        sgr_color(&mut self.text, foreground, 38);
        sgr_color(&mut self.text, self.background, 48);
        self.text.push_str(style);
        self.text.push_str(&text);
        if let Some(target) = target {
            self.hit(start, target);
        }
    }

    fn hit(&mut self, start: usize, target: Target) {
        if start < self.column {
            self.hits.push((start..self.column, target));
        }
    }

    fn space(&mut self, cells: usize) {
        let cells = cells.min(self.columns.saturating_sub(self.column));
        self.push(&" ".repeat(cells), self.foreground, "", None);
    }

    fn finish(mut self, row: isize) -> (String, Vec<Hit>) {
        self.space(self.columns.saturating_sub(self.column));
        self.text.push_str("\x1b[0m");
        let hits = self
            .hits
            .into_iter()
            .map(|(columns, target)| Hit {
                row,
                columns,
                target,
            })
            .collect();
        (self.text, hits)
    }
}

// Keep the theme hue and move toward its foreground only when needed for text contrast.
fn readable(
    color: PaletteColor,
    background: PaletteColor,
    foreground: PaletteColor,
) -> PaletteColor {
    let (PaletteColor::Rgb(color), PaletteColor::Rgb(background), PaletteColor::Rgb(foreground)) =
        (color, background, foreground)
    else {
        return color;
    };
    let background = luminance(background);
    for step in 0..=16u16 {
        let mix = |a, b| ((u16::from(a) * (16 - step) + u16::from(b) * step) / 16) as u8;
        let candidate = (
            mix(color.0, foreground.0),
            mix(color.1, foreground.1),
            mix(color.2, foreground.2),
        );
        let value = luminance(candidate);
        if (value.max(background) + 0.05) / (value.min(background) + 0.05) >= 4.5 {
            return PaletteColor::Rgb(candidate);
        }
    }
    PaletteColor::Rgb(foreground)
}

fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let [r, g, b] = [r, g, b].map(|value| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn sgr_color(output: &mut String, color: Option<PaletteColor>, which: u8) {
    match color {
        Some(PaletteColor::Rgb((r, g, b))) => {
            write!(output, "\x1b[{which};2;{r};{g};{b}m").expect("writing to a String");
        }
        Some(PaletteColor::EightBit(value)) => {
            write!(output, "\x1b[{which};5;{value}m").expect("writing to a String");
        }
        None => {}
    }
}

fn widget_width(widgets: &[(&Widget, String)]) -> usize {
    widgets
        .iter()
        .map(|(widget, text)| widget.gap + width(text))
        .sum()
}

fn join(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (true, _) => second.to_owned(),
        (_, true) => first.to_owned(),
        (false, false) => format!("{first} {second}"),
    }
}

fn width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

pub(crate) fn clean(value: &str) -> String {
    let printable: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    printable.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(value: &str, columns: usize) -> String {
    if width(value) <= columns {
        return value.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    let mut remaining = columns - 1;
    let mut clipped = String::new();
    for grapheme in value.graphemes(true) {
        let cells = width(grapheme);
        if cells > remaining {
            break;
        }
        remaining -= cells;
        clipped.push_str(grapheme);
    }
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_cells_without_splitting_a_grapheme() {
        assert_eq!(clip("界a\u{0301}bc", 4), "界a\u{0301}…");
        assert_eq!(clip("界a", 1), "…");
    }

    #[test]
    fn removes_terminal_control_characters_from_data() {
        assert_eq!(clean("feature\nnext\u{001b}[0m"), "feature next [0m");
    }
    #[test]
    fn keeps_active_tab_and_clickable_actions_when_width_changes() {
        let config = crate::config::Config::parse(
            r#"
            widget priority=8 { text "Fri 16:40"; }
            widget priority=1 { text "optional-stat"; }
            button "first" label="First tool" icon="▸" on-icon="▪" shortcut="f" {
                action { ToggleSessionCard; }
            }
            button "second" label="Second tool" shortcut="Space" { action { ToggleSessionCard; }; }
            button "third" label="Third tool" shortcut="t" { action { ToggleSessionCard; }; }
        "#,
        )
        .expect("valid render specimen");
        let tabs = [
            Tab {
                id: 1,
                number: 1,
                name: "Experiment",
                icon: "",
                active: false,
            },
            Tab {
                id: 2,
                number: 2,
                name: "Features",
                icon: "",
                active: true,
            },
            Tab {
                id: 3,
                number: 3,
                name: "Review worktree",
                icon: "",
                active: false,
            },
        ];
        let values = ["Fri 16:40".into(), "optional-stat".into()];
        let view = View {
            theme: Some(Styling::default()),
            tabs: &tabs,
            tab_options: &config.tabs,
            widgets: &config.widgets,
            values: &values,
            buttons: &config.buttons,
            open: &[true, false, false],
            hover: Some(Target::Button(0)),
            error: None,
        };
        let plain = |out: &str| String::from_utf8(strip_ansi_escapes::strip(out)).expect("text");
        let (wide, hits) = view.render(2, 88);
        let wide = plain(&wide);
        assert!(wide.contains("▪ First tool (f)"));
        assert!(wide.contains("Second tool (Space)"));
        assert!(wide.contains("Review worktree"));
        assert_eq!(hits.len(), 6);
        let (narrow, hits) = view.render(2, 31);
        let narrow = plain(&narrow);
        assert!(narrow.contains("Features"));
        assert!(!narrow.contains("optional-stat"));
        assert!(narrow.contains('…'));
        assert!(hits.iter().any(|hit| hit.target == Target::Tab(2)));
        assert!(narrow.lines().all(|line| width(line) == 31));
        let (tiny, _) = view.render(2, 7);
        assert!(plain(&tiny).contains('…'));
    }
}
