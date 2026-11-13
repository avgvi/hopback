// One dark palette with a name for each role. Agent colors come from
// `AgentId::color`.

use ratatui::style::Color;

use crate::config::AgentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Theme {
    pub(super) foreground: Color,
    pub(super) accent: Color,
    pub(super) warning: Color,
    pub(super) error: Color,
    pub(super) success: Color,
    pub(super) info: Color,
    pub(super) muted: Color,
    pub(super) secondary: Color,
    pub(super) panel_border: Color,
    pub(super) selected_fg: Color,
    pub(super) selected_bg: Color,
    pub(super) key_fg: Color,
    pub(super) key_bg: Color,
    /// Text on the one accent-colored chip, the main action.
    pub(super) key_primary_fg: Color,
    pub(super) user_accent: Color,
    pub(super) user_text: Color,
    pub(super) assistant_text: Color,
    pub(super) match_fg: Color,
    pub(super) match_bg: Color,
    pub(super) pin: Color,
    pub(super) fork: Color,
}

pub(super) const THEME: Theme = Theme {
    foreground: Color::White,
    accent: Color::Rgb(224, 150, 70),
    warning: Color::Rgb(240, 180, 80),
    error: Color::Rgb(240, 100, 100),
    success: Color::Green,
    info: Color::Cyan,
    // Fixed colors, not the terminal's own grays: those change with every
    // terminal theme and are often too dim to read on a dark background.
    muted: Color::Rgb(128, 138, 152),
    secondary: Color::Rgb(176, 184, 196),
    panel_border: Color::Rgb(70, 80, 95),
    selected_fg: Color::White,
    selected_bg: Color::Rgb(68, 52, 34),
    key_fg: Color::Rgb(230, 234, 240),
    key_bg: Color::Rgb(42, 50, 61),
    key_primary_fg: Color::Rgb(26, 18, 6),
    user_accent: Color::Rgb(120, 210, 255),
    user_text: Color::Rgb(180, 225, 245),
    assistant_text: Color::Rgb(220, 225, 230),
    match_fg: Color::Black,
    match_bg: Color::Rgb(250, 220, 110),
    pin: Color::Rgb(250, 220, 110),
    fork: Color::Rgb(120, 210, 255),
};

pub(super) fn agent_color(agent: AgentId) -> Color {
    let c = agent.color();
    Color::Rgb(c.0, c.1, c.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_agent_has_a_visible_color() {
        for agent in AgentId::ALL {
            let Color::Rgb(r, g, b) = agent_color(agent) else {
                panic!("agent colors are RGB");
            };
            let brightest = r.max(g).max(b);
            assert!(brightest >= 120, "{} is too dark to read", agent.as_str());
        }
    }

    #[test]
    fn selection_and_match_colors_differ_from_their_text() {
        assert_ne!(THEME.selected_bg, THEME.selected_fg);
        assert_ne!(THEME.match_bg, THEME.match_fg);
        assert_ne!(THEME.key_bg, THEME.key_fg);
    }
}
