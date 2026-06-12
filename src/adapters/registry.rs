use super::claude::ClaudeAdapter;
use super::codex::CodexAdapter;
use super::copilot::CopilotAdapter;
use super::copilot_vscode::CopilotVscodeAdapter;
use super::crush::CrushAdapter;
use super::opencode::OpenCodeAdapter;
use super::vibe::VibeAdapter;
use super::Adapter;

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ClaudeAdapter::new()),
        Box::new(CodexAdapter::new()),
        Box::new(CopilotAdapter::new()),
        Box::new(CopilotVscodeAdapter::new()),
        Box::new(VibeAdapter::new()),
        Box::new(CrushAdapter::new()),
        Box::new(OpenCodeAdapter::new()),
    ]
}

pub fn available() -> Vec<Box<dyn Adapter>> {
    all().into_iter().filter(|a| a.is_available()).collect()
}
