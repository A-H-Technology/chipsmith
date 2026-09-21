use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ChipsmithError {
    #[error("process `{command}` failed with exit code {code:?}{}",
            stderr_tail.as_deref().map(|t| format!("\n{t}")).unwrap_or_default())]
    ProcessFailed {
        command: String,
        code: Option<i32>,
        /// The tail of what the process said before it gave up, when it was
        /// captured. `None` means the output went straight to the terminal.
        stderr_tail: Option<String>,
    },

    #[error("failed to spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("toolchain not installed at {path}")]
    NotInstalled { path: PathBuf },

    #[error("installer not found: {path}")]
    InstallerNotFound { path: PathBuf },

    #[error("unknown tool: {name} (available: {})", available.join(", "))]
    UnknownTool {
        name: String,
        available: Vec<String>,
    },

    #[error("chipsmith.toml not found: {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("chipsmith.toml parse error in {path}: {message}")]
    ManifestParse { path: PathBuf, message: String },

    #[error("no source files matched pattern: {pattern}")]
    NoSourceFiles { pattern: String },

    #[error("unknown version: {version} (available: {})", available.join(", "))]
    UnknownVersion {
        version: String,
        available: Vec<String>,
    },

    #[error("unknown toolchain backend: {name} (available: {})", available.join(", "))]
    UnknownBackend {
        name: String,
        available: Vec<String>,
    },

    #[error("nix eval failed: {message}")]
    NixEvalFailed { message: String },

    #[error("could not read nix eval output: {message}")]
    NixEvalOutput { message: String },

    #[error(
        "nixpkgs has no usable `{attr}`, which chipsmith needs to run vendor binaries on NixOS"
    )]
    NixPackageMissing { attr: String },

    #[error("output file not found: {path} (have you run `chipsmith build`?)")]
    OutputNotFound { path: PathBuf },

    #[error("chipsmith.toml already exists: {path}")]
    ProjectAlreadyExists { path: PathBuf },

    /// Carries the URL so a retired CDN path (a 404) is distinguishable from a
    /// transport failure without reading the message.
    #[error("download failed: {url}{}: {message}",
            status.map(|s| format!(" (HTTP {s})")).unwrap_or_default())]
    Download {
        url: String,
        status: Option<u16>,
        message: String,
    },

    #[error("could not {operation} {path}: {source}")]
    File {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ChipsmithError {
    /// An I/O failure that knows which file it was about. Prefer this over the
    /// bare `Io` conversion anywhere the path is the useful half of the news.
    pub fn file(
        operation: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::File {
            operation,
            path,
            source,
        }
    }
}
