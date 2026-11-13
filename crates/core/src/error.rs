use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("failed to parse mirror status JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid regular expression: {0}")]
    Regex(#[from] regex::Error),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("config error: {0}")]
    Config(String),
    #[error("privileged helper failed: {0}")]
    Helper(String),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}
