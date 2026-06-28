use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitEvent {
    pub initiated_by_user: bool,
    pub message: Option<String>,
}

impl TerminalExitEvent {
    pub fn user_initiated() -> Self {
        Self {
            initiated_by_user: true,
            message: None,
        }
    }

    pub fn unexpected(message: impl Into<String>) -> Self {
        Self {
            initiated_by_user: false,
            message: Some(message.into()),
        }
    }
}
