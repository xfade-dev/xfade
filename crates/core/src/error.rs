use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("config parse failed for {path}: {msg}")]
    ConfigParse { path: String, msg: String },

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("missing API key: {0}")]
    MissingApiKey(String),

    #[error("cannot remove active provider: {0}")]
    ActiveProviderRemoval(String),

    #[error("secret not found: {0}")]
    SecretNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml deserialize error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("`xfade run` doesn't support {0} (config-file driven; use `xfade use` instead)")]
    RunUnsupported(String),

    #[error("proxy error: {0}")]
    Proxy(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
