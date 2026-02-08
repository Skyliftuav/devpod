use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevpodConfig {
    pub project: ProjectConfig,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub registry: RegistryConfig,
    pub infrastructure: InfrastructureConfig,
    pub deployment: DeploymentConfig,
    #[serde(default)]
    pub network: NetworkConfig,
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
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: DevpodConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
