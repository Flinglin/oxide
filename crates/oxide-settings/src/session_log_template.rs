const MAX_FILE_NAME_TEMPLATE_CHARS: usize = 256;
const MAX_CONTENT_TEMPLATE_CHARS: usize = 1024;
const MAX_DIRECTORY_TEMPLATE_CHARS: usize = 512;
const MAX_DIRECTORY_TEMPLATE_COMPONENTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSessionLogTemplateVariable {
    Date,
    Time,
    DateTime,
    Timestamp,
    Session,
    Host,
    Username,
    Protocol,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalSessionLogTemplatePart {
    Literal(String),
    Variable(TerminalSessionLogTemplateVariable),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTerminalSessionLogTemplate {
    parts: Vec<TerminalSessionLogTemplatePart>,
}

impl ParsedTerminalSessionLogTemplate {
    pub fn parts(&self) -> &[TerminalSessionLogTemplatePart] {
        &self.parts
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTerminalSessionLogDirectoryTemplate {
    components: Vec<ParsedTerminalSessionLogTemplate>,
}

impl ParsedTerminalSessionLogDirectoryTemplate {
    pub fn components(&self) -> &[ParsedTerminalSessionLogTemplate] {
        &self.components
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalSessionLogTemplateError {
    Empty,
    TooLong,
    UnclosedVariable,
    UnexpectedClosingBrace,
    UnknownVariable,
    MissingText,
    RepeatedText,
    InvalidFileNameCharacter,
}

pub fn parse_terminal_session_log_file_name_template(
    template: &str,
) -> Result<ParsedTerminalSessionLogTemplate, TerminalSessionLogTemplateError> {
    parse_template(template, TemplateKind::FileName)
}

pub fn parse_terminal_session_log_content_template(
    template: &str,
) -> Result<ParsedTerminalSessionLogTemplate, TerminalSessionLogTemplateError> {
    parse_template(template, TemplateKind::Content)
}

pub fn parse_terminal_session_log_directory_template(
    template: &str,
) -> Result<ParsedTerminalSessionLogDirectoryTemplate, TerminalSessionLogTemplateError> {
    if template.is_empty() {
        return Ok(ParsedTerminalSessionLogDirectoryTemplate {
            components: Vec::new(),
        });
    }
    if template.chars().count() > MAX_DIRECTORY_TEMPLATE_CHARS {
        return Err(TerminalSessionLogTemplateError::TooLong);
    }
    if template.starts_with('/') || template.ends_with('/') || template.contains('\\') {
        return Err(TerminalSessionLogTemplateError::InvalidFileNameCharacter);
    }

    let components = template
        .split('/')
        .map(|component| {
            if matches!(component, "" | "." | "..") {
                return Err(TerminalSessionLogTemplateError::InvalidFileNameCharacter);
            }
            parse_template(component, TemplateKind::FileName)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() > MAX_DIRECTORY_TEMPLATE_COMPONENTS {
        return Err(TerminalSessionLogTemplateError::TooLong);
    }
    Ok(ParsedTerminalSessionLogDirectoryTemplate { components })
}

#[derive(Clone, Copy)]
enum TemplateKind {
    FileName,
    Content,
}

fn parse_template(
    template: &str,
    kind: TemplateKind,
) -> Result<ParsedTerminalSessionLogTemplate, TerminalSessionLogTemplateError> {
    if template.is_empty() {
        return Err(TerminalSessionLogTemplateError::Empty);
    }
    let max_chars = match kind {
        TemplateKind::FileName => MAX_FILE_NAME_TEMPLATE_CHARS,
        TemplateKind::Content => MAX_CONTENT_TEMPLATE_CHARS,
    };
    if template.chars().count() > max_chars {
        return Err(TerminalSessionLogTemplateError::TooLong);
    }

    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();
    let mut text_count = 0;
    while let Some(character) = chars.next() {
        match character {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    parts.push(TerminalSessionLogTemplatePart::Literal(std::mem::take(
                        &mut literal,
                    )));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => {
                            return Err(TerminalSessionLogTemplateError::UnclosedVariable);
                        }
                        Some(character) => name.push(character),
                    }
                }
                let variable = parse_variable(&name)
                    .ok_or(TerminalSessionLogTemplateError::UnknownVariable)?;
                if !variable_allowed(variable, kind) {
                    return Err(TerminalSessionLogTemplateError::UnknownVariable);
                }
                if variable == TerminalSessionLogTemplateVariable::Text {
                    text_count += 1;
                }
                parts.push(TerminalSessionLogTemplatePart::Variable(variable));
            }
            '}' => return Err(TerminalSessionLogTemplateError::UnexpectedClosingBrace),
            character => {
                if matches!(kind, TemplateKind::FileName)
                    && (character.is_control()
                        || matches!(
                            character,
                            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
                        ))
                {
                    return Err(TerminalSessionLogTemplateError::InvalidFileNameCharacter);
                }
                literal.push(character);
            }
        }
    }
    if !literal.is_empty() {
        parts.push(TerminalSessionLogTemplatePart::Literal(literal));
    }

    if matches!(kind, TemplateKind::Content) {
        match text_count {
            0 => return Err(TerminalSessionLogTemplateError::MissingText),
            1 => {}
            _ => return Err(TerminalSessionLogTemplateError::RepeatedText),
        }
    }
    Ok(ParsedTerminalSessionLogTemplate { parts })
}

fn parse_variable(name: &str) -> Option<TerminalSessionLogTemplateVariable> {
    match name {
        "date" => Some(TerminalSessionLogTemplateVariable::Date),
        "time" => Some(TerminalSessionLogTemplateVariable::Time),
        "datetime" => Some(TerminalSessionLogTemplateVariable::DateTime),
        "timestamp" => Some(TerminalSessionLogTemplateVariable::Timestamp),
        "session" => Some(TerminalSessionLogTemplateVariable::Session),
        "host" => Some(TerminalSessionLogTemplateVariable::Host),
        "username" => Some(TerminalSessionLogTemplateVariable::Username),
        "protocol" => Some(TerminalSessionLogTemplateVariable::Protocol),
        "text" => Some(TerminalSessionLogTemplateVariable::Text),
        _ => None,
    }
}

fn variable_allowed(variable: TerminalSessionLogTemplateVariable, kind: TemplateKind) -> bool {
    match kind {
        TemplateKind::FileName => !matches!(
            variable,
            TerminalSessionLogTemplateVariable::Timestamp
                | TerminalSessionLogTemplateVariable::Text
        ),
        TemplateKind::Content => true,
    }
}
