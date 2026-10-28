use std::{error::Error, fmt};

#[derive(Debug)]
pub struct EngineError(String);

impl EngineError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for EngineError {}

impl From<unicorn_engine::uc_error> for EngineError {
    fn from(error: unicorn_engine::uc_error) -> Self {
        Self::new(format!("ARM execution: {error:?}"))
    }
}

impl From<std::io::Error> for EngineError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}
