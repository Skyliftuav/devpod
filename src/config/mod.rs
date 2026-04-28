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
    #[serde(default)]
    pub access: ClusterAccessConfig,
    #[serde(default)]
    pub tailscale: TailscaleConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteNodeConfig {
    pub role: String, // server, agent
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bootstrap_address: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default = "default_runtime")]
    pub runtime: String, // containerd, docker
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

fn default_runtime() -> String {
    "containerd".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterAccessConfig {
    #[serde(default = "default_access_mode")]
    pub mode: String,
    #[serde(default = "default_access_primary")]
    pub primary: String,
    #[serde(default = "default_lan_domain")]
    pub lan_domain: String,
    #[serde(default)]
    pub published_ports: Vec<PublishedPortConfig>,
}

fn default_access_mode() -> String {
    "dual".to_string()
}

fn default_access_primary() -> String {
    "tailscale".to_string()
}

fn default_lan_domain() -> String {
    "local".to_string()
}

impl Default for ClusterAccessConfig {
    fn default() -> Self {
        Self {
            mode: default_access_mode(),
            primary: default_access_primary(),
            lan_domain: default_lan_domain(),
            published_ports: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedPortConfig {
    pub node: String,
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailscaleConfig {
    #[serde(default = "default_tailscale_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tailnet_domain: Option<String>,
    #[serde(default = "default_tailscale_auth_key_env")]
    pub auth_key_env: String,
    #[serde(default = "default_tailscale_api_key_env")]
    pub api_key_env: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_tailscale_ssh")]
    pub ssh: bool,
}

fn default_tailscale_enabled() -> bool {
    false
}

fn default_tailscale_auth_key_env() -> String {
    "TAILSCALE_AUTH_KEY".to_string()
}

fn default_tailscale_api_key_env() -> String {
    "TAILSCALE_API_KEY".to_string()
}

fn default_tailscale_ssh() -> bool {
    true
}

impl Default for TailscaleConfig {
    fn default() -> Self {
        Self {
            enabled: default_tailscale_enabled(),
            tailnet_domain: None,
            auth_key_env: default_tailscale_auth_key_env(),
            api_key_env: default_tailscale_api_key_env(),
            tags: Vec::new(),
            ssh: default_tailscale_ssh(),
        }
    }
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

    // Helper to get cluster config by environment name
    pub fn get_cluster(&self, env: &str) -> Option<&ClusterDefinition> {
        self.cluster.get(env)
    }
}

impl ClusterDefinition {
    pub fn tailscale_enabled(&self) -> bool {
        self.tailscale.enabled && self.access_mode() != "lan-only"
    }

    pub fn access_mode(&self) -> &str {
        self.access.mode.as_str()
    }

    pub fn prefers_tailscale(&self) -> bool {
        self.access.primary == "tailscale" && self.tailscale_enabled()
    }

    pub fn prefers_lan(&self) -> bool {
        self.access.primary == "lan" || !self.tailscale_enabled()
    }

    pub fn lan_domain(&self) -> &str {
        self.access.lan_domain.as_str()
    }

    pub fn tailnet_domain(&self) -> Option<&str> {
        self.tailscale
            .tailnet_domain
            .as_deref()
            .filter(|domain| !domain.trim().is_empty())
    }
}

impl RemoteNodeConfig {
    pub fn bootstrap_address(&self) -> Option<&str> {
        self.bootstrap_address
            .as_deref()
            .or(self.address.as_deref())
            .filter(|value| !value.trim().is_empty())
    }

    pub fn stable_name(&self) -> String {
        let raw = self
            .name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.bootstrap_address())
            .unwrap_or(self.role.as_str());

        raw.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    }

    pub fn lan_hostname(&self, domain: &str) -> String {
        format!("{}.{}", self.stable_name(), domain.trim_start_matches('.'))
    }

    pub fn tailscale_hostname(&self, tailnet_domain: &str) -> String {
        format!(
            "{}.{}",
            self.stable_name(),
            tailnet_domain.trim_start_matches('.')
        )
    }

    pub fn matches_node_ref(&self, value: &str) -> bool {
        self.stable_name() == value
            || self
                .name
                .as_deref()
                .map(|name| name == value)
                .unwrap_or(false)
            || self
                .bootstrap_address()
                .map(|address| address == value)
                .unwrap_or(false)
            || self
                .address
                .as_deref()
                .map(|address| address == value)
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{ClusterDefinition, RemoteNodeConfig, TailscaleConfig};
    use std::collections::HashMap;

    #[test]
    fn remote_node_bootstrap_address_falls_back_to_legacy_address() {
        let node = RemoteNodeConfig {
            role: "server".to_string(),
            name: None,
            bootstrap_address: None,
            address: Some("192.168.1.10".to_string()),
            runtime: "containerd".to_string(),
            labels: HashMap::new(),
        };

        assert_eq!(node.bootstrap_address(), Some("192.168.1.10"));
        assert_eq!(node.stable_name(), "192-168-1-10");
    }

    #[test]
    fn cluster_defaults_fall_back_to_lan_until_tailscale_is_enabled() {
        let cluster = ClusterDefinition {
            provider: "k3s".to_string(),
            connection: None,
            user: None,
            nodes: Vec::new(),
            datastore_endpoint: None,
            access: Default::default(),
            tailscale: TailscaleConfig::default(),
        };

        assert!(!cluster.tailscale_enabled());
        assert!(!cluster.prefers_tailscale());
        assert!(cluster.prefers_lan());
    }
}
