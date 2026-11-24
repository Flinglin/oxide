use serde::{Deserialize, Serialize};

use crate::{X11AuthCommand, X11AuthorityFile, X11Display, X11ForwardingError, X11Result};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11AuthorityEnvironment {
    pub display: Option<String>,
    pub authority_file: X11AuthorityFile,
}

impl X11AuthorityEnvironment {
    pub fn from_process_env() -> Self {
        Self::from_values(
            std::env::var("DISPLAY").ok(),
            std::env::var("XAUTHORITY").ok(),
        )
    }

    pub fn from_values(display: Option<String>, xauthority: Option<String>) -> Self {
        Self {
            display: display.and_then(non_empty_string),
            authority_file: xauthority
                .and_then(non_empty_string)
                .map(X11AuthorityFile::Path)
                .unwrap_or(X11AuthorityFile::Default),
        }
    }

    pub fn parse_display(&self) -> X11Result<X11Display> {
        let display = self
            .display
            .as_ref()
            .ok_or(X11ForwardingError::MissingDisplay)?;
        X11Display::parse(display)
    }

    pub fn xauth_list_command(&self) -> X11Result<X11AuthCommand> {
        Ok(X11AuthCommand::list(
            &self.parse_display()?,
            self.authority_file.clone(),
        ))
    }

    pub fn xauth_nlist_command(&self) -> X11Result<X11AuthCommand> {
        Ok(X11AuthCommand::nlist(
            &self.parse_display()?,
            self.authority_file.clone(),
        ))
    }
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
