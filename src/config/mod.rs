use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevpodConfig {
    pub project: ProjectConfig,

    // Legacy support (optional now as we prefer cluster map)
    #[serde(default)]
    pub provider: Option<ProviderConfig>,

    // New cluster map support
    #[serde(default)]
    pub cluster: HashMap<String, ClusterDefinition>,

    #[serde(default)]
    pub registry: RegistryConfig,
    pub infrastructure: InfrastructureConfig,
    pub deployment: DeploymentConfig,
    #[serde(default)]
    pub network: NetworkConfig,

    #[serde(default)]
    pub secrets: SecretsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_secrets_tool")]
    pub tool: String,
    pub set: Option<String>,
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

fn default_secrets_tool() -> String {
    "ksecret".to_string()
}

fn default_namespace() -> String {
    "default".to_string()
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tool: default_secrets_tool(),
            set: None,
            namespace: default_namespace(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterDefinition {
    pub provider: String, // k3d, k3s
    #[serde(default)]
    pub connection: Option<String>, // ssh
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub nodes: Vec<RemoteNodeConfig>,
    #[serde(default)]
    pub datastore_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNodeConfig {
    pub role: String, // server, agent
    pub address: String,
    #[serde(default = "default_runtime")]
    pub runtime: String, // containerd, docker
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

fn default_runtime() -> String {
    "containerd".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    #[serde(default = "default_registry_enabled")]
    pub enabled: bool,
    #[serde(default = "default_registry_port")]
    pub port: u16,
}

fn default_registry_enabled() -> bool {
    true
}

fn default_registry_port() -> u16 {
    32000
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            enabled: default_registry_enabled(),
            port: default_registry_port(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type", default = "default_provider_type")]
    pub provider_type: String,
}

fn default_provider_type() -> String {
    "auto".to_string()
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: default_provider_type(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfrastructureConfig {
    #[serde(default)]
    pub persistent_storage_enabled: bool,
    #[serde(default = "default_data_path")]
    pub data_mount_path: String,
}

fn default_data_path() -> String {
    "/var/lib/devpod/storage".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentConfig {
    pub tool: String,
    pub environment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetworkConfig {
    #[serde(default)]
    pub expose: Vec<PortMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host: u16,
    pub container: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

impl DevpodConfig {
    pub fn load(path: &str) -> crate::error::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: DevpodConfig = toml::from_str(&content).map_err(|e| {
            crate::error::DevpodError::Config(format!(
                "failed to parse config file '{}': {}",
                path, e
            ))
        })?;
        Ok(config)
    }

    // Helper to get cluster config by environment name
    pub fn get_cluster(&self, env: &str) -> Option<&ClusterDefinition> {
        self.cluster.get(env)
    }
}
