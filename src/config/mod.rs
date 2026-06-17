use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use colored::Colorize;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevpodConfig {
    #[serde(default)]
    pub schema_version: u32,

    pub project: ProjectConfig,

    // Legacy support (optional now as we prefer cluster map)
    #[serde(default)]
    pub provider: Option<ProviderConfig>,

    // New cluster map support
    #[serde(default)]
    pub cluster: HashMap<String, ClusterDefinition>,

    #[serde(default)]
    pub cluster_defaults: Option<ClusterDefinition>,

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
    #[serde(default = "default_cluster_provider")]
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

fn default_cluster_provider() -> String {
    "k3s".to_string()
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
        let mut config: DevpodConfig = toml::from_str(&content)?;

        // Validate schema version
        if config.schema_version > 1 {
            anyhow::bail!(
                "Unsupported config schema version: {}. Supported versions: 0, 1. Please upgrade devpod.",
                config.schema_version
            );
        }
        if config.schema_version == 0 {
            println!(
                "{} Warning: Config schema version is 0 (legacy). Please run 'devpod config migrate' to upgrade.",
                "WARN".yellow()
            );
        }

        // Find parent directory of the config file
        let config_path = std::path::Path::new(path);
        let base_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));

        // Scan base_dir/clusters and base_dir/.devpod/clusters
        let dirs_to_scan = vec![
            base_dir.join("clusters"),
            base_dir.join(".devpod").join("clusters"),
        ];

        // A wrapper struct to try parsing files as multiple clusters:
        // [cluster.name]
        // ...
        #[derive(Deserialize)]
        struct MultiClusterFile {
            #[serde(default)]
            cluster: HashMap<String, ClusterDefinition>,
        }

        for dir in dirs_to_scan {
            if dir.is_dir() {
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    let file_path = entry.path();
                    if file_path.is_file() && file_path.extension().and_then(|s| s.to_str()) == Some("toml") {
                        let file_content = std::fs::read_to_string(&file_path)?;
                        
                        // Try parsing as multi-cluster wrapper first
                        let mut loaded = false;
                        if let Ok(multi) = toml::from_str::<MultiClusterFile>(&file_content) {
                            if !multi.cluster.is_empty() {
                                for (name, cluster_def) in multi.cluster {
                                    config.cluster.insert(name, cluster_def);
                                }
                                loaded = true;
                            }
                        }

                        if !loaded {
                            // Otherwise try parsing as a single flat ClusterDefinition
                            match toml::from_str::<ClusterDefinition>(&file_content) {
                                Ok(cluster_def) => {
                                    if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                                        config.cluster.insert(stem.to_string(), cluster_def);
                                    }
                                }
                                Err(err) => {
                                    anyhow::bail!(
                                        "Failed to parse cluster config file {}: {}",
                                        file_path.display(),
                                        err
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Merge defaults
        if let Some(ref defaults) = config.cluster_defaults {
            for (_, cluster) in config.cluster.iter_mut() {
                cluster.merge_defaults(defaults);
            }
        }

        Ok(config)
    }

    pub fn migrate(path: &str) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(path)?;
        let mut config: DevpodConfig = toml::from_str(&content)?;

        if config.schema_version > 0 {
            anyhow::bail!("Configuration is already migrated (schema_version = {})", config.schema_version);
        }

        let config_path = std::path::Path::new(path);
        let base_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let clusters_dir = base_dir.join("clusters");
        std::fs::create_dir_all(&clusters_dir)?;

        for (name, cluster_def) in &config.cluster {
            let cluster_content = toml::to_string(cluster_def)?;
            let cluster_file = clusters_dir.join(format!("{}.toml", name));
            std::fs::write(cluster_file, cluster_content)?;
            println!("  {} Migrated cluster context to clusters/{}.toml", "OK".green(), name);
        }

        // Update config to schema version 1 and clear the embedded cluster map
        config.schema_version = 1;
        config.cluster.clear();

        // Save back to base config
        let updated_content = toml::to_string(&config)?;
        std::fs::write(path, updated_content)?;
        Ok(())
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

    pub fn merge_defaults(&mut self, defaults: &ClusterDefinition) {
        if self.provider == "k3s" && defaults.provider != "k3s" {
            self.provider = defaults.provider.clone();
        }
        if self.connection.is_none() {
            self.connection = defaults.connection.clone();
        }
        if self.user.is_none() {
            self.user = defaults.user.clone();
        }
        if self.datastore_endpoint.is_none() {
            self.datastore_endpoint = defaults.datastore_endpoint.clone();
        }
        
        // Merge access
        if self.access.mode == "dual" && defaults.access.mode != "dual" {
            self.access.mode = defaults.access.mode.clone();
        }
        if self.access.primary == "tailscale" && defaults.access.primary != "tailscale" {
            self.access.primary = defaults.access.primary.clone();
        }
        if self.access.lan_domain == "local" && defaults.access.lan_domain != "local" {
            self.access.lan_domain = defaults.access.lan_domain.clone();
        }
        if self.access.published_ports.is_empty() {
            self.access.published_ports = defaults.access.published_ports.clone();
        }

        // Merge tailscale
        if !self.tailscale.enabled && defaults.tailscale.enabled {
            self.tailscale.enabled = true;
        }
        if self.tailscale.tailnet_domain.is_none() {
            self.tailscale.tailnet_domain = defaults.tailscale.tailnet_domain.clone();
        }
        if self.tailscale.auth_key_env == "TAILSCALE_AUTH_KEY" && defaults.tailscale.auth_key_env != "TAILSCALE_AUTH_KEY" {
            self.tailscale.auth_key_env = defaults.tailscale.auth_key_env.clone();
        }
        if self.tailscale.api_key_env == "TAILSCALE_API_KEY" && defaults.tailscale.api_key_env != "TAILSCALE_API_KEY" {
            self.tailscale.api_key_env = defaults.tailscale.api_key_env.clone();
        }
        if self.tailscale.tags.is_empty() {
            self.tailscale.tags = defaults.tailscale.tags.clone();
        }
        if self.tailscale.ssh && !defaults.tailscale.ssh {
            self.tailscale.ssh = false;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DevpodState {
    pub active_environment: Option<String>,
}

impl DevpodState {
    pub fn load(config_path: &str) -> Self {
        let path = std::path::Path::new(config_path);
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let state_path = base_dir.join(".devpod").join("state.toml");
        
        if let Ok(content) = std::fs::read_to_string(state_path) {
            if let Ok(state) = toml::from_str::<DevpodState>(&content) {
                return state;
            }
        }
        DevpodState::default()
    }

    pub fn save(&self, config_path: &str) -> anyhow::Result<()> {
        let path = std::path::Path::new(config_path);
        let base_dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let devpod_dir = base_dir.join(".devpod");
        
        std::fs::create_dir_all(&devpod_dir)?;
        let state_path = devpod_dir.join("state.toml");
        let content = toml::to_string(self)?;
        std::fs::write(state_path, content)?;
        Ok(())
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

    #[test]
    fn test_merge_defaults() {
        let defaults = ClusterDefinition {
            provider: "k3d".to_string(),
            connection: Some("ssh".to_string()),
            user: Some("dev-user".to_string()),
            nodes: Vec::new(),
            datastore_endpoint: None,
            access: super::ClusterAccessConfig {
                mode: "tailscale-only".to_string(),
                primary: "tailscale".to_string(),
                lan_domain: "defaults.local".to_string(),
                published_ports: Vec::new(),
            },
            tailscale: TailscaleConfig {
                enabled: true,
                tailnet_domain: Some("defaults.ts.net".to_string()),
                auth_key_env: "TEST_AUTH_KEY".to_string(),
                api_key_env: "TEST_API_KEY".to_string(),
                tags: vec!["tag:test".to_string()],
                ssh: false,
            },
        };

        let mut cluster = ClusterDefinition {
            provider: "k3s".to_string(),
            connection: None,
            user: None,
            nodes: Vec::new(),
            datastore_endpoint: None,
            access: Default::default(),
            tailscale: TailscaleConfig::default(),
        };

        cluster.merge_defaults(&defaults);

        assert_eq!(cluster.provider, "k3d"); // Inherited defaults provider
        assert_eq!(cluster.connection, Some("ssh".to_string())); // Merged connection
        assert_eq!(cluster.user, Some("dev-user".to_string())); // Merged user
        assert_eq!(cluster.access.mode, "tailscale-only"); // Merged access mode
        assert_eq!(cluster.access.lan_domain, "defaults.local"); // Merged lan domain
        assert!(cluster.tailscale.enabled); // Merged tailscale enabled
        assert_eq!(cluster.tailscale.tailnet_domain, Some("defaults.ts.net".to_string())); // Merged tailnet
        assert_eq!(cluster.tailscale.auth_key_env, "TEST_AUTH_KEY"); // Merged auth key env
        assert_eq!(cluster.tailscale.api_key_env, "TEST_API_KEY"); // Merged api key env
        assert_eq!(cluster.tailscale.tags, vec!["tag:test".to_string()]); // Merged tags
        assert!(!cluster.tailscale.ssh); // Merged ssh setting

        // Test that an explicit non-default provider is not overwritten
        let mut cluster_custom = ClusterDefinition {
            provider: "custom".to_string(),
            connection: None,
            user: None,
            nodes: Vec::new(),
            datastore_endpoint: None,
            access: Default::default(),
            tailscale: TailscaleConfig::default(),
        };
        cluster_custom.merge_defaults(&defaults);
        assert_eq!(cluster_custom.provider, "custom");
    }

    #[test]
    fn test_devpod_state() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("devpod.toml");
        std::fs::write(&config_path, "").unwrap();

        let mut state = super::DevpodState::default();
        state.active_environment = Some("test-env".to_string());
        state.save(config_path.to_str().unwrap()).unwrap();

        let loaded = super::DevpodState::load(config_path.to_str().unwrap());
        assert_eq!(loaded.active_environment, Some("test-env".to_string()));
    }
}
