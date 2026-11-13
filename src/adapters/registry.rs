use super::antigravity::AntigravityAdapter;
use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::copilot::CopilotAdapter;
use super::copilot_vscode::CopilotVscodeAdapter;
use super::crush::CrushAdapter;
use super::cursor::CursorAdapter;
use super::gemini::GeminiAdapter;
use super::goose::GooseAdapter;
use super::grok::GrokAdapter;
use super::kimi::KimiAdapter;
use super::kiro::KiroAdapter;
use super::opencode::OpenCodeAdapter;
use super::pi::PiAdapter;
use super::vibe::VibeAdapter;
use super::Adapter;

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(AntigravityAdapter::new()),
        Box::new(ClaudeAdapter::new()),
        Box::new(CodexAdapter::new()),
        Box::new(CopilotAdapter::new()),
        Box::new(CopilotVscodeAdapter::new()),
        Box::new(CrushAdapter::new()),
        Box::new(CursorAdapter::new()),
        Box::new(GeminiAdapter::new()),
        Box::new(GooseAdapter::new()),
        Box::new(GrokAdapter::new()),
        Box::new(KimiAdapter::new()),
        Box::new(KiroAdapter::new()),
        Box::new(OpenCodeAdapter::new()),
        Box::new(PiAdapter::new()),
        Box::new(VibeAdapter::new()),
    ]
}

pub fn available() -> Vec<Box<dyn Adapter>> {
    all().into_iter().filter(|a| a.is_available()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;

    #[test]
    fn every_agent_id_has_exactly_one_adapter() {
        let ids: Vec<AgentId> = all().iter().map(|a| a.id()).collect();
        assert_eq!(ids, AgentId::ALL.to_vec());
    }
}
