use thiserror::Error;

#[derive(Error, Debug)]
pub enum DevpodError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Network error: {0}")]
    Network(String),

    // #[error("Template parsing error: {0}")]
    // Template(String),

    // #[error("Serialization error: {0}")]
    // Serialize(String),

    // #[error("Orchestrator error: {0}")]
    // Orchestration(String),
    #[error("Execution error on {host}: {msg}")]
    Execution { host: String, msg: String },

    #[error("Cluster definition for '{0}' not found in config")]
    ClusterNotFound(String),

    #[error("Kubeconfig merge failed: {0}")]
    KubeconfigMerge(String),

    #[error("Failed to parse YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Command execution failed: {0}")]
    Command(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, DevpodError>;
