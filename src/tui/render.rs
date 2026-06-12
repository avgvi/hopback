use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use super::app::{AppView, ForkChoice, PermissionChoice};
use super::format;
use crate::config::{AgentId, Rgb};
use crate::search::{self, SearchResult};

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

const AGENT_W: u16 = 9;
const TURNS_W: u16 = 6;
const DATE_W: u16 = 14;

const DIR_MIN: u16 = 14;
const DIR_MAX: u16 = 36;

pub(super) fn draw(f: &mut Frame, app: &super::app::App) {
    let view = app.view();
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title_bar(f, chunks[0], &view);
    draw_search(f, chunks[1], &view);
    draw_filter_bar(f, chunks[2], &view);
    draw_body(f, chunks[3], &view);
    draw_footer(f, chunks[4], &view);

    if let Some((agent, action, selected)) = view.permission_modal {
        draw_permission_modal(f, area, agent, action, selected);
    }
    if let Some(input) = view.rename_input {
        draw_rename_modal(f, area, input);
    }
    if let Some((agent, choices, selected)) = view.fork_modal {
        draw_fork_modal(f, area, agent, choices, selected);
    }
    if view.hide_modal {
        draw_hide_modal(f, area);
    }
}

fn draw_hide_modal(f: &mut Frame, area: Rect) {
    let w = 64u16.min(area.width.saturating_sub(2));
    let h = 7u16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" hide session ");

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Hide this session from aiz?",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  Source agent files are unchanged.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  enter hide · esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_rename_modal(f: &mut Frame, area: Rect, input: &str) {
    let w = 60u16.min(area.width.saturating_sub(2));
    let h = 5u16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" rename session ");

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(input, Style::default().fg(Color::White)),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(Span::styled(
            "  enter save · esc cancel · ^u clear · empty resets to original",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_permission_modal(f: &mut Frame, area: Rect, agent: &str, action: &str, selected: usize) {
    let w = 72u16.min(area.width.saturating_sub(2));
    let h = 12u16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(ratatui::widgets::Clear, popup);

    let title = format!(" {action} {agent} — permissions ");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title);

    let sel = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let unsel = Style::default().fg(Color::Gray);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  How should aiz handle skip-permission mode?",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
    ];
    for (i, choice) in PermissionChoice::ALL.iter().enumerate() {
        let style = if i == selected { sel } else { unsel };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} {}", i + 1, choice.label()), style),
            Span::styled(
                format!(" — {}", choice.description()),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  enter choose · arrows/tab move · 1-5 choose · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_fork_modal(
    f: &mut Frame,
    area: Rect,
    agent: &str,
    choices: &[ForkChoice],
    selected: usize,
) {
    let w = 56u16.min(area.width.saturating_sub(2));
    let h = (choices.len() as u16 + 4)
        .clamp(6, 10)
        .min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" fork target ");

    let mut lines = vec![Line::from(Span::styled(
        format!("  Fork {agent} session as:"),
        Style::default().fg(Color::Gray),
    ))];
    for (i, choice) in choices.iter().enumerate() {
        let label = match choice {
            ForkChoice::Native => format!("native {agent} fork"),
            ForkChoice::Translated(target) => format!("translated {} fork", target.badge()),
        };
        let marker = if i == selected { ">" } else { " " };
        let style = if i == selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{marker} {label}"), style),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "  enter choose · tab/down next · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn draw_title_bar(f: &mut Frame, area: Rect, view: &AppView) {
    let title = format!("aiz v{}", env!("CARGO_PKG_VERSION"));
    let count = if view.indexing {
        format!("{} sessions · indexing…", view.total)
    } else if view.shown < view.total {
        format!("{}/{} sessions", view.shown, view.total)
    } else {
        format!("{} sessions", view.total)
    };

    let pad =
        (area.width as usize).saturating_sub(title.chars().count() + count.chars().count() + 2);
    let line = Line::from(vec![
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(count, Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_search(f: &mut Frame, area: Rect, view: &AppView) {
    let title = if view.query_ms > 0.0 {
        format!(" search · {:.1}ms ", view.query_ms)
    } else {
        " search ".to_string()
    };
    let input = Paragraph::new(Line::from(vec![
        Span::styled("🔍 ", Style::default().fg(Color::Cyan)),
        Span::raw(view.query),
    ]))
    .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(input, area);

    let cx = area.x + 4 + view.query.chars().count() as u16;
    let cy = area.y + 1;
    f.set_cursor_position((cx.min(area.x + area.width.saturating_sub(2)), cy));
}

fn draw_filter_bar(f: &mut Frame, area: Rect, view: &AppView) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));
    spans.push(chip("All", None, view.active_filter.is_none(), Color::Gray));

    for agent in view.agents_present {
        spans.push(Span::raw("  "));
        let active = view.active_filter == Some(*agent);
        spans.push(chip(
            agent.badge(),
            Some(*agent),
            active,
            rgb(agent.color()),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn chip<'a>(label: &'a str, agent: Option<AgentId>, active: bool, color: Color) -> Span<'a> {
    let dot = if agent.is_some() { "● " } else { "" };
    let text = format!(" {dot}{label} ");
    if active {
        Span::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(text, Style::default().fg(color))
    }
}

fn draw_body(f: &mut Frame, area: Rect, view: &AppView) {
    if view.show_preview {
        let preview_h = view.preview_height.min(area.height.saturating_sub(3));
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(preview_h)])
            .split(area);
        draw_results(f, rows[0], view);
        draw_preview(f, rows[1], view);
    } else {
        draw_results(f, area, view);
    }
}

fn dir_width(body_width: u16) -> u16 {
    let fixed = AGENT_W + TURNS_W + DATE_W + 4;
    let avail = body_width.saturating_sub(fixed + 20);
    avail
        .clamp(0, DIR_MAX)
        .max(if body_width < 70 { 0 } else { DIR_MIN })
}

fn draw_results(f: &mut Frame, area: Rect, view: &AppView) {
    let now = format::now_unix_secs();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir_w = dir_width(area.width);
    let show_dir = dir_w > 0;

    let rows: Vec<Row> = if view.results.is_empty() {
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                "No sessions found",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        ])]
    } else {
        view.results
            .iter()
            .map(|r| {
                let pinned = view.pinned.contains(&r.session.key());
                result_row(r, view.query, now, &home, dir_w, pinned)
            })
            .collect()
    };

    let widths: Vec<Constraint> = if show_dir {
        vec![
            Constraint::Length(AGENT_W),
            Constraint::Min(20),
            Constraint::Length(dir_w),
            Constraint::Length(TURNS_W),
            Constraint::Length(DATE_W),
        ]
    } else {
        vec![
            Constraint::Length(AGENT_W),
            Constraint::Min(20),
            Constraint::Length(TURNS_W),
            Constraint::Length(DATE_W),
        ]
    };
    let header_cells: Vec<Cell> = if show_dir {
        vec![
            Cell::from("Agent"),
            Cell::from("Title"),
            Cell::from("Directory"),
            Cell::from("Turns"),
            Cell::from("Date"),
        ]
    } else {
        vec![
            Cell::from("Agent"),
            Cell::from("Title"),
            Cell::from("Turns"),
            Cell::from("Date"),
        ]
    };

    let table = Table::new(rows, widths)
        .header(
            Row::new(header_cells).style(
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(Block::default().borders(Borders::ALL))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(48, 48, 56))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");

    let mut state = TableState::default();
    if !view.results.is_empty() {
        state.select(Some(view.selected));
    }
    f.render_stateful_widget(table, area, &mut state);
}

fn result_row<'a>(
    r: &'a SearchResult,
    query: &str,
    now: i64,
    home: &str,
    dir_w: u16,
    pinned: bool,
) -> Row<'a> {
    let s = &r.session;
    let badge_cell = Cell::from(Span::styled(
        s.agent.badge().to_string(),
        Style::default()
            .fg(rgb(s.agent.color()))
            .add_modifier(Modifier::BOLD),
    ));

    let title_cell = if pinned {
        let mut spans = vec![Span::styled("★ ", Style::default().fg(Color::Yellow))];
        spans.extend(highlight(&s.title, query).spans);
        Cell::from(Line::from(spans))
    } else {
        Cell::from(highlight(&s.title, query))
    };

    let turns = if s.message_count > 0 {
        s.message_count.to_string()
    } else {
        "-".to_string()
    };
    let turns_cell = Cell::from(Line::from(turns).alignment(Alignment::Right));

    let age_hours = ((now - s.timestamp).max(0) as f64) / 3600.0;
    let date_cell = Cell::from(Span::styled(
        format::time_ago(now, s.timestamp),
        Style::default().fg(rgb(format::age_color(age_hours))),
    ));

    if dir_w > 0 {
        let dir = format::truncate_left(
            &format::format_directory(&s.directory, home),
            dir_w as usize,
        );
        Row::new(vec![
            badge_cell,
            title_cell,
            Cell::from(Span::styled(dir, Style::default().fg(Color::DarkGray))),
            turns_cell,
            date_cell,
        ])
    } else {
        Row::new(vec![badge_cell, title_cell, turns_cell, date_cell])
    }
}

fn lower_with_map(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut lc = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    let mut tmp = [0u8; 4];
    for (orig_off, ch) in text.char_indices() {
        let orig_end = orig_off + ch.len_utf8();
        for lo in ch.to_lowercase() {
            let s = lo.encode_utf8(&mut tmp);
            for _ in 0..s.len() {
                map.push((orig_off, orig_end));
            }
            lc.push_str(s);
        }
    }
    (lc, map)
}

fn highlight<'a>(text: &'a str, query: &str) -> Line<'a> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Line::from(text);
    }

    let (lc, map) = lower_with_map(text);
    let mut spans: Vec<Span> = Vec::new();
    let hl = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut i = 0;
    let mut last = 0;
    while i < lc.len() {
        let next = terms
            .iter()
            .filter_map(|t| lc[i..].find(t.as_str()).map(|rel| (i + rel, t.len())))
            .min_by_key(|(pos, _)| *pos);
        match next {
            Some((pos, len)) => {
                let orig_start = map[pos].0;
                let last_byte = (pos + len).saturating_sub(1).min(map.len() - 1);
                let orig_end = map[last_byte].1.max(orig_start);
                if orig_start > last {
                    spans.push(Span::raw(&text[last..orig_start]));
                }
                spans.push(Span::styled(text[orig_start..orig_end].to_string(), hl));
                last = orig_end;

                i = resume_lc_index(&map, orig_end).max(i + 1);
            }
            None => break,
        }
    }
    if last < text.len() {
        spans.push(Span::raw(&text[last..]));
    }
    Line::from(spans)
}

fn resume_lc_index(map: &[(usize, usize)], orig: usize) -> usize {
    map.iter()
        .position(|&(s, _)| s >= orig)
        .unwrap_or(map.len())
}

fn draw_preview(f: &mut Frame, area: Rect, view: &AppView) {
    let block = Block::default().borders(Borders::ALL).title(" preview ");
    let Some(r) = view.results.get(view.selected) else {
        f.render_widget(Paragraph::new("").block(block), area);
        return;
    };
    let s = &r.session;

    let content = &s.content;
    let start = search::first_match_offset(content, view.query)
        .map(|off| off.saturating_sub(200))
        .unwrap_or(0);
    let start = floor_char_boundary(content, start);
    let snippet: String = content[start..].chars().take(5000).collect();

    let home = std::env::var("HOME").unwrap_or_default();
    let meta = Line::from(vec![
        Span::styled(
            s.agent.badge().to_string(),
            Style::default()
                .fg(rgb(s.agent.color()))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format::format_directory(&s.directory, &home),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("  ·  {} turns", s.message_count),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let mut lines = vec![meta, Line::from("")];
    for raw in snippet.lines() {
        lines.push(preview_line(raw, view.query, s.agent));
    }

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn preview_line<'a>(raw: &'a str, query: &str, agent: AgentId) -> Line<'a> {
    use crate::model::{ASSISTANT_PREFIX, USER_PREFIX};

    if let Some(rest) = raw.strip_prefix(USER_PREFIX) {
        let mut spans = vec![Span::styled(
            "» ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(highlight(rest, query).spans);
        Line::from(spans)
    } else if let Some(rest) = raw.strip_prefix(ASSISTANT_PREFIX) {
        let mut spans = vec![Span::styled("● ", Style::default().fg(rgb(agent.color())))];
        spans.extend(highlight(rest, query).spans);
        Line::from(spans)
    } else {
        highlight(raw, query)
    }
}

fn draw_footer(f: &mut Frame, area: Rect, view: &AppView) {
    let help = if view.can_fork_selected {
        " enter Resume · ^f Fork · ^x Hide · ^p Pin · ^r Rename · ^y Copy · esc Quit"
    } else {
        " enter Resume · ^x Hide · ^p Pin · ^r Rename · ^y Copy · ^` Preview · esc Quit"
    };
    let mut spans = vec![Span::styled(help, Style::default().fg(Color::DarkGray))];
    if let Some(toast) = view.toast {
        if !view.indexing {
            let used = help.chars().count() + toast.chars().count();
            let pad = (area.width as usize).saturating_sub(used + 1);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(toast, Style::default().fg(Color::Green)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_marks_matched_terms() {
        let line = highlight("authentication middleware", "auth");
        let texts: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(texts.iter().any(|t| t.eq_ignore_ascii_case("auth")));
    }

    #[test]
    fn highlight_no_query_is_plain() {
        let line = highlight("hello world", "");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn highlight_does_not_panic_on_length_changing_multibyte() {
        for (text, q) in [
            ("ẞẞẞ match", "match"),
            ("İx", "i"),
            ("aİbİc match", "i"),
            ("\u{212A}elvin match", "match"),
        ] {
            let line = highlight(text, q);

            let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(
                joined, text,
                "spans must reconstruct the input for {text:?}"
            );
        }
    }

    #[test]
    fn highlight_reconstructs_and_marks_ascii() {
        let line = highlight("authentication middleware", "auth mid");
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "authentication middleware");

        let styled: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Yellow))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(styled.iter().any(|t| t.eq_ignore_ascii_case("auth")));
        assert!(styled.iter().any(|t| t.eq_ignore_ascii_case("mid")));
    }

    #[test]
    fn floor_char_boundary_is_safe_on_multibyte() {
        let s = "αβγδε";
        let b = floor_char_boundary(s, 3);
        assert!(s.is_char_boundary(b));
        assert!(b <= 3);
    }

    #[test]
    fn dir_width_hides_on_narrow_terminals() {
        assert_eq!(dir_width(50), 0);
        assert!(dir_width(120) >= DIR_MIN);
        assert!(dir_width(300) <= DIR_MAX);
    }

    #[test]
    fn preview_line_marks_user_and_assistant() {
        let u = preview_line("» hello there", "", AgentId::Claude);
        assert_eq!(u.spans[0].content, "» ");
        let a = preview_line("  on it", "", AgentId::Claude);
        assert_eq!(a.spans[0].content, "● ");
    }
}
