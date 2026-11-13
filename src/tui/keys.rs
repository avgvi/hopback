//! Key bindings.
//!
//! Typing always goes to the search line, so every action needs a modifier or
//! a non-character key. `map_key` is the whole mapping for the main screen;
//! `KEY_HELP` is the same list in words, for the F1 overlay.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows moved by Page Up and Page Down.
pub(super) const PAGE: isize = 10;

/// Rows or lines moved by one notch of the mouse wheel.
pub(super) const WHEEL_LINES: isize = 3;

/// An edit to the search line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditOp {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Ctrl+W: delete the word before the cursor.
    DeleteWord,
    /// Ctrl+U: delete everything before the cursor.
    DeleteToStart,
}

/// What a key press means on the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    Resume,
    Move(isize),
    NextTab,
    PrevTab,
    Fork,
    /// Ctrl+O: the actions menu for the selected session.
    OpenActions,
    /// Ctrl+T: pin the selected session to the top, or unpin it.
    Pin,
    /// Ctrl+R or F2: rename the selected session inside hopback.
    Rename,
    /// Ctrl+X: hide the selected session from hopback.
    Hide,
    TogglePreview,
    Copy,
    Help,
    /// Esc: clear the search, or quit when it is already empty.
    Escape,
    /// Ctrl+C: quit at once.
    Quit,
    Edit(EditOp),
    None,
}

pub(super) fn map_key(key: KeyEvent) -> Command {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Enter => Command::Resume,
        KeyCode::Esc => Command::Escape,
        KeyCode::F(1) => Command::Help,
        KeyCode::F(2) => Command::Rename,

        KeyCode::Up => Command::Move(-1),
        KeyCode::Down => Command::Move(1),
        KeyCode::PageUp => Command::Move(-PAGE),
        KeyCode::PageDown => Command::Move(PAGE),

        KeyCode::BackTab => Command::PrevTab,
        KeyCode::Tab if shift => Command::PrevTab,
        KeyCode::Tab => Command::NextTab,

        KeyCode::Left => Command::Edit(EditOp::Left),
        KeyCode::Right => Command::Edit(EditOp::Right),
        KeyCode::Home => Command::Edit(EditOp::Home),
        KeyCode::End => Command::Edit(EditOp::End),
        KeyCode::Backspace => Command::Edit(EditOp::Backspace),
        KeyCode::Delete => Command::Edit(EditOp::Delete),

        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            'c' => Command::Quit,
            'f' => Command::Fork,
            'o' => Command::OpenActions,
            't' => Command::Pin,
            'r' => Command::Rename,
            'x' => Command::Hide,
            'p' => Command::TogglePreview,
            'y' => Command::Copy,
            'a' => Command::Edit(EditOp::Home),
            'e' => Command::Edit(EditOp::End),
            'w' => Command::Edit(EditOp::DeleteWord),
            'u' => Command::Edit(EditOp::DeleteToStart),
            _ => Command::None,
        },
        KeyCode::Char(c) if !alt => Command::Edit(EditOp::Insert(c)),
        _ => Command::None,
    }
}

/// An entry of the actions menu (Ctrl+O), which acts on the selected session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuAction {
    Fork,
    Pin,
    Rename,
    Hide,
    Copy,
}

impl MenuAction {
    /// In the order the menu shows them: what is used most comes first.
    pub(super) const ALL: [MenuAction; 5] = [
        MenuAction::Pin,
        MenuAction::Rename,
        MenuAction::Fork,
        MenuAction::Copy,
        MenuAction::Hide,
    ];

    /// The key that does the same thing without opening the menu. The menu
    /// shows it beside the entry, so the menu teaches the shortcuts.
    pub(super) const fn shortcut(self) -> &'static str {
        match self {
            MenuAction::Pin => "Ctrl+T",
            MenuAction::Rename => "Ctrl+R",
            MenuAction::Fork => "Ctrl+F",
            MenuAction::Copy => "Ctrl+Y",
            MenuAction::Hide => "Ctrl+X",
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            MenuAction::Pin => "Pin to the top",
            MenuAction::Rename => "Rename",
            MenuAction::Fork => "Fork into a new session",
            MenuAction::Copy => "Copy the resume command",
            MenuAction::Hide => "Hide from hopback",
        }
    }

    /// The menu entry a main-screen command stands for, so a shortcut also
    /// works while the menu is open.
    pub(super) const fn from_command(command: Command) -> Option<MenuAction> {
        match command {
            Command::Pin => Some(MenuAction::Pin),
            Command::Rename => Some(MenuAction::Rename),
            Command::Fork => Some(MenuAction::Fork),
            Command::Copy => Some(MenuAction::Copy),
            Command::Hide => Some(MenuAction::Hide),
            _ => None,
        }
    }
}

/// Every binding, grouped, as shown by F1: (keys, what they do).
pub(super) const KEY_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Sessions",
        &[
            ("Enter", "Resume the selected session"),
            ("↑ ↓ / PgUp PgDn", "Move one row, or ten"),
            ("Tab / Shift+Tab", "Next or previous agent tab"),
            ("Ctrl+T", "Pin to the top, or unpin"),
            ("Ctrl+R / F2", "Rename inside hopback"),
            ("Ctrl+F", "Fork the selected session"),
            ("Ctrl+Y", "Copy the resume command"),
            ("Ctrl+X", "Hide from hopback"),
            ("Ctrl+O", "Menu of these actions"),
            ("Ctrl+P", "Preview on or off"),
        ],
    ),
    (
        "Search",
        &[
            ("type", "Search titles, messages and paths"),
            ("agent: dir: date:", "Filter, as in agent:claude date:week"),
            ("Tab / →", "Finish a suggested agent or date"),
            ("← → Home End", "Move the cursor"),
            ("Ctrl+A / Ctrl+E", "Start or end of the line"),
            ("Backspace / Delete", "Delete a character"),
            ("Ctrl+W", "Delete the word before the cursor"),
            ("Ctrl+U", "Delete to the start of the line"),
            ("Esc", "Clear the search; quit when it is empty"),
        ],
    ),
    (
        "Mouse",
        &[
            ("Click / Double-click", "Select a session / resume it"),
            ("Right-click", "Actions menu for that session"),
            ("Wheel", "Scroll the list, or the preview under the pointer"),
        ],
    ),
    ("Other", &[("F1", "This help"), ("Ctrl+C", "Quit")]),
];

/// Footer chips: (key, action), in the order shown. The Esc entry reads
/// "clear" while there is a search to clear, and the pin entry "unpin" on a
/// pinned session.
pub(super) fn footer_keys(
    query_is_empty: bool,
    can_fork: bool,
    selected_is_pinned: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![("Enter", "resume")];
    if can_fork {
        keys.push(("Ctrl+F", "fork"));
    }
    keys.extend([
        ("Ctrl+T", if selected_is_pinned { "unpin" } else { "pin" }),
        ("Ctrl+R", "rename"),
        ("Ctrl+O", "more"),
        ("F1", "help"),
        ("Esc", if query_is_empty { "quit" } else { "clear" }),
    ]);
    keys
}

/// Chips dropped, first to last, when the footer is wider than the screen.
/// Everything here is also in the Ctrl+O menu and the F1 help.
const FOOTER_DROP_ORDER: [&str; 4] = ["Ctrl+F", "Ctrl+R", "Ctrl+T", "Ctrl+O"];

/// Cells one chip takes: " key " and " action", plus two cells between chips.
fn chip_width(key: &str, action: &str) -> usize {
    key.chars().count() + action.chars().count() + 3
}

/// Cells the footer takes for these chips, with its one-cell left margin.
pub(super) fn footer_width(keys: &[(&str, &str)]) -> usize {
    let chips: usize = keys.iter().map(|(k, a)| chip_width(k, a)).sum();
    1 + chips + 2 * keys.len().saturating_sub(1)
}

/// `keys` without the least important chips, until the rest fits in `width`.
pub(super) fn fit_footer(
    mut keys: Vec<(&'static str, &'static str)>,
    width: usize,
) -> Vec<(&'static str, &'static str)> {
    for drop in FOOTER_DROP_ORDER {
        if footer_width(&keys) <= width {
            break;
        }
        keys.retain(|(key, _)| *key != drop);
    }
    keys
}

fn byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Applies an edit to the search text. `cursor` is a character index and is
/// kept inside the text. Returns true when the text itself changed.
pub(super) fn apply_edit(text: &mut String, cursor: &mut usize, op: EditOp) -> bool {
    let len = text.chars().count();
    *cursor = (*cursor).min(len);
    match op {
        EditOp::Insert(c) => {
            text.insert(byte_index(text, *cursor), c);
            *cursor += 1;
            true
        }
        EditOp::Backspace => {
            if *cursor == 0 {
                return false;
            }
            *cursor -= 1;
            text.remove(byte_index(text, *cursor));
            true
        }
        EditOp::Delete => {
            if *cursor >= len {
                return false;
            }
            text.remove(byte_index(text, *cursor));
            true
        }
        EditOp::Left => {
            *cursor = cursor.saturating_sub(1);
            false
        }
        EditOp::Right => {
            *cursor = (*cursor + 1).min(len);
            false
        }
        EditOp::Home => {
            *cursor = 0;
            false
        }
        EditOp::End => {
            *cursor = len;
            false
        }
        EditOp::DeleteWord => {
            let chars: Vec<char> = text.chars().collect();
            let mut start = *cursor;
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            while start > 0 && !chars[start - 1].is_whitespace() {
                start -= 1;
            }
            if start == *cursor {
                return false;
            }
            text.replace_range(byte_index(text, start)..byte_index(text, *cursor), "");
            *cursor = start;
            true
        }
        EditOp::DeleteToStart => {
            if *cursor == 0 {
                return false;
            }
            text.replace_range(..byte_index(text, *cursor), "");
            *cursor = 0;
            true
        }
    }
}

/// The part of the search text that fits in `width` cells with the cursor in
/// view, and the cursor's column inside that part.
pub(super) fn input_view(text: &str, cursor: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    // Keep one cell free after the last character for the cursor itself.
    let start = (cursor + 1).saturating_sub(width);
    let visible: String = chars.iter().skip(start).take(width).collect();
    (visible, cursor - start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn the_spec_key_table() {
        assert_eq!(map_key(key(KeyCode::Enter)), Command::Resume);
        assert_eq!(map_key(key(KeyCode::Up)), Command::Move(-1));
        assert_eq!(map_key(key(KeyCode::Down)), Command::Move(1));
        assert_eq!(map_key(key(KeyCode::PageUp)), Command::Move(-PAGE));
        assert_eq!(map_key(key(KeyCode::PageDown)), Command::Move(PAGE));
        assert_eq!(map_key(key(KeyCode::Tab)), Command::NextTab);
        assert_eq!(map_key(key(KeyCode::BackTab)), Command::PrevTab);
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
            Command::PrevTab
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Command::PrevTab
        );
        assert_eq!(map_key(ctrl('f')), Command::Fork);
        assert_eq!(map_key(ctrl('o')), Command::OpenActions);
        assert_eq!(map_key(ctrl('p')), Command::TogglePreview);
        assert_eq!(map_key(ctrl('y')), Command::Copy);
        assert_eq!(map_key(key(KeyCode::F(1))), Command::Help);
        assert_eq!(map_key(key(KeyCode::Esc)), Command::Escape);
        assert_eq!(map_key(ctrl('c')), Command::Quit);
    }

    #[test]
    fn search_line_editing_keys() {
        assert_eq!(map_key(key(KeyCode::Left)), Command::Edit(EditOp::Left));
        assert_eq!(map_key(key(KeyCode::Right)), Command::Edit(EditOp::Right));
        assert_eq!(map_key(key(KeyCode::Home)), Command::Edit(EditOp::Home));
        assert_eq!(map_key(key(KeyCode::End)), Command::Edit(EditOp::End));
        assert_eq!(map_key(ctrl('a')), Command::Edit(EditOp::Home));
        assert_eq!(map_key(ctrl('e')), Command::Edit(EditOp::End));
        assert_eq!(map_key(ctrl('w')), Command::Edit(EditOp::DeleteWord));
        assert_eq!(map_key(ctrl('u')), Command::Edit(EditOp::DeleteToStart));
        assert_eq!(map_key(key(KeyCode::Delete)), Command::Edit(EditOp::Delete));
        assert_eq!(
            map_key(key(KeyCode::Backspace)),
            Command::Edit(EditOp::Backspace)
        );
    }

    #[test]
    fn typing_always_goes_to_the_search() {
        for c in [
            'a', 'f', 'p', 'r', 'h', 'y', 'q', 'x', '?', '/', ' ', '.', '1', 'é',
        ] {
            assert_eq!(
                map_key(key(KeyCode::Char(c))),
                Command::Edit(EditOp::Insert(c)),
                "{c:?} must type"
            );
        }
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT)),
            Command::Edit(EditOp::Insert('F'))
        );
    }

    #[test]
    fn removed_bindings_do_nothing() {
        // Ctrl+` was the old preview toggle.
        assert_eq!(map_key(ctrl('`')), Command::None);
        assert_ne!(map_key(ctrl('p')), Command::None, "Ctrl+P is preview now");
        // Ctrl+Up and Ctrl+Down no longer resize anything; they just move.
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            Command::Move(-1)
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
            Command::None
        );
    }

    #[test]
    fn ctrl_letters_ignore_case() {
        assert_eq!(
            map_key(KeyEvent::new(
                KeyCode::Char('F'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Command::Fork
        );
    }

    #[test]
    fn pin_rename_and_hide_have_their_own_keys() {
        assert_eq!(map_key(ctrl('t')), Command::Pin);
        assert_eq!(map_key(ctrl('r')), Command::Rename);
        assert_eq!(map_key(key(KeyCode::F(2))), Command::Rename);
        assert_eq!(map_key(ctrl('x')), Command::Hide);
        // The plain letters still type into the search.
        for c in ['t', 'r', 'x'] {
            assert_eq!(
                map_key(key(KeyCode::Char(c))),
                Command::Edit(EditOp::Insert(c))
            );
        }
    }

    #[test]
    fn every_menu_entry_shows_the_key_that_runs_it() {
        for action in MenuAction::ALL {
            let shortcut = action.shortcut();
            let letter = shortcut
                .strip_prefix("Ctrl+")
                .unwrap()
                .chars()
                .next()
                .unwrap();
            let command = map_key(ctrl(letter.to_ascii_lowercase()));
            assert_eq!(
                MenuAction::from_command(command),
                Some(action),
                "{shortcut}"
            );
        }
        assert_eq!(MenuAction::from_command(Command::Resume), None);
        assert_eq!(MenuAction::ALL[0], MenuAction::Pin);
        assert_eq!(MenuAction::ALL[1], MenuAction::Rename);
    }

    #[test]
    fn footer_puts_pin_and_rename_on_screen() {
        assert_eq!(
            footer_keys(true, true, false),
            vec![
                ("Enter", "resume"),
                ("Ctrl+F", "fork"),
                ("Ctrl+T", "pin"),
                ("Ctrl+R", "rename"),
                ("Ctrl+O", "more"),
                ("F1", "help"),
                ("Esc", "quit"),
            ]
        );
        assert_eq!(
            footer_keys(false, true, false).last(),
            Some(&("Esc", "clear"))
        );
        assert!(!footer_keys(true, false, false).contains(&("Ctrl+F", "fork")));
        assert!(footer_keys(true, true, true).contains(&("Ctrl+T", "unpin")));
    }

    #[test]
    fn footer_drops_chips_until_it_fits() {
        let all = footer_keys(true, true, false);
        let full = footer_width(&all);
        assert_eq!(
            fit_footer(all.clone(), full),
            all,
            "nothing dropped when it fits"
        );
        assert!(
            full <= 100,
            "the whole footer fits a 100-column screen: {full}"
        );

        let narrow = fit_footer(all.clone(), 80);
        assert!(footer_width(&narrow) <= 80);
        assert!(!narrow.contains(&("Ctrl+F", "fork")), "fork goes first");
        assert!(narrow.contains(&("Ctrl+T", "pin")), "pin stays longer");

        let tiny = fit_footer(all, 40);
        assert_eq!(
            tiny,
            vec![("Enter", "resume"), ("F1", "help"), ("Esc", "quit")]
        );
    }

    #[test]
    fn help_lists_every_bound_key() {
        let listed: String = KEY_HELP
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(keys, _)| *keys))
            .collect::<Vec<_>>()
            .join(" | ");
        for expected in [
            "Enter",
            "↑ ↓",
            "PgUp",
            "PgDn",
            "Tab",
            "Shift+Tab",
            "Ctrl+F",
            "Ctrl+O",
            "Ctrl+P",
            "Ctrl+Y",
            "Ctrl+T",
            "Ctrl+R",
            "F2",
            "Ctrl+X",
            "Right-click",
            "F1",
            "Esc",
            "←",
            "→",
            "Home",
            "End",
            "Ctrl+A",
            "Ctrl+E",
            "Ctrl+W",
            "Ctrl+U",
            "Delete",
            "Backspace",
            "Ctrl+C",
            "Click",
            "Double-click",
            "Wheel",
        ] {
            assert!(listed.contains(expected), "{expected} missing from help");
        }
        for action in MenuAction::ALL {
            assert!(listed.contains(action.shortcut()), "{}", action.shortcut());
        }
    }

    fn edit(text: &str, cursor: usize, ops: &[EditOp]) -> (String, usize) {
        let mut text = text.to_string();
        let mut cursor = cursor;
        for op in ops {
            apply_edit(&mut text, &mut cursor, *op);
        }
        (text, cursor)
    }

    #[test]
    fn insert_and_delete_at_the_cursor() {
        assert_eq!(edit("ac", 1, &[EditOp::Insert('b')]), ("abc".into(), 2));
        assert_eq!(edit("abc", 3, &[EditOp::Insert('d')]), ("abcd".into(), 4));
        assert_eq!(edit("abc", 2, &[EditOp::Backspace]), ("ac".into(), 1));
        assert_eq!(edit("abc", 0, &[EditOp::Backspace]), ("abc".into(), 0));
        assert_eq!(edit("abc", 1, &[EditOp::Delete]), ("ac".into(), 1));
        assert_eq!(edit("abc", 3, &[EditOp::Delete]), ("abc".into(), 3));
    }

    #[test]
    fn cursor_movement_stays_in_bounds() {
        assert_eq!(edit("abc", 0, &[EditOp::Left]).1, 0);
        assert_eq!(edit("abc", 3, &[EditOp::Right]).1, 3);
        assert_eq!(edit("abc", 1, &[EditOp::Right, EditOp::Right]).1, 3);
        assert_eq!(edit("abc", 2, &[EditOp::Home]).1, 0);
        assert_eq!(edit("abc", 0, &[EditOp::End]).1, 3);
        // A stale cursor past the end is pulled back in.
        assert_eq!(edit("abc", 99, &[EditOp::Insert('d')]), ("abcd".into(), 4));
    }

    #[test]
    fn delete_word_and_delete_to_start() {
        assert_eq!(
            edit("fix the auth", 12, &[EditOp::DeleteWord]),
            ("fix the ".into(), 8)
        );
        assert_eq!(
            edit("fix the  ", 9, &[EditOp::DeleteWord]),
            ("fix ".into(), 4)
        );
        assert_eq!(
            edit("fix the auth", 7, &[EditOp::DeleteWord]),
            ("fix  auth".into(), 4)
        );
        assert_eq!(edit("abc", 0, &[EditOp::DeleteWord]), ("abc".into(), 0));
        assert_eq!(
            edit("agent:claude auth", 13, &[EditOp::DeleteToStart]),
            ("auth".into(), 0)
        );
        assert_eq!(edit("abc", 3, &[EditOp::DeleteToStart]), (String::new(), 0));
    }

    #[test]
    fn edits_are_unicode_safe() {
        assert_eq!(edit("héllo", 2, &[EditOp::Backspace]), ("hllo".into(), 1));
        assert_eq!(edit("αβγ", 1, &[EditOp::Insert('δ')]), ("αδβγ".into(), 2));
        assert_eq!(edit("αβ γδ", 5, &[EditOp::DeleteWord]), ("αβ ".into(), 3));
        assert_eq!(
            edit("日本語", 3, &[EditOp::Left, EditOp::Delete]),
            ("日本".into(), 2)
        );
    }

    #[test]
    fn apply_edit_reports_text_changes_only() {
        let mut text = "abc".to_string();
        let mut cursor = 3;
        assert!(!apply_edit(&mut text, &mut cursor, EditOp::Left));
        assert!(!apply_edit(&mut text, &mut cursor, EditOp::Home));
        assert!(!apply_edit(&mut text, &mut cursor, EditOp::Backspace));
        assert!(apply_edit(&mut text, &mut cursor, EditOp::Insert('x')));
        assert!(apply_edit(&mut text, &mut cursor, EditOp::Delete));
    }

    #[test]
    fn input_view_keeps_the_cursor_visible() {
        assert_eq!(input_view("hello", 5, 20), ("hello".into(), 5));
        assert_eq!(input_view("hello", 2, 20), ("hello".into(), 2));
        // Too long for the box: the view slides so the cursor stays inside.
        let (visible, col) = input_view("abcdefghij", 10, 5);
        assert_eq!(visible, "ghij");
        assert_eq!(col, 4);
        let (visible, col) = input_view("abcdefghij", 0, 5);
        assert_eq!(visible, "abcde");
        assert_eq!(col, 0);
        let (visible, col) = input_view("abcdefghij", 6, 5);
        assert_eq!(visible, "cdefg");
        assert_eq!(col, 4);
        assert_eq!(input_view("abc", 1, 0), (String::new(), 0));
    }
}
