use crate::config::DevpodConfig;
use crate::error::{DevpodError, Result};
use crate::orchestrator::ClusterManager;
use async_trait::async_trait;
use std::net::TcpListener;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::info;
use walkdir::WalkDir;

pub struct K3dManager {
    name: String,
    registry_port: u16,
}

impl K3dManager {
    pub fn new(name: &str, registry_port: u16) -> Self {
        Self {
            name: name.to_string(),
            registry_port,
        }
    }
}

#[async_trait]
impl ClusterManager for K3dManager {
    async fn up(&self, config: &DevpodConfig) -> Result<()> {
        info!("Provisioning k3d cluster '{}'...", self.name);

        // Check if Docker is running
        let docker_check = Command::new("docker")
            .arg("info")
            .output()
            .await
            .map_err(|e| {
                DevpodError::Command(format!(
                    "Failed to run docker info. Is Docker installed? {}",
                    e
                ))
            })?;

        if !docker_check.status.success() {
            return Err(DevpodError::Command(
                "Docker is not running. Please start Docker Desktop/OrbStack.".into(),
            ));
        }

        // Validate host ports before asking k3d to create the cluster.
        // This avoids long create+rollback cycles when a bind would fail.
        for mapping in &config.network.expose {
            match TcpListener::bind(("0.0.0.0", mapping.host)) {
                Ok(_) => {}
                Err(e) => {
                    let detail = match e.kind() {
                        std::io::ErrorKind::AddrInUse => format!(
                            "Port {} is already in use (network.expose host port).\n\
                             Remediation: free the port (`lsof -i :{}`), or update [network].expose host value in devpod.toml.",
                            mapping.host, mapping.host
                        ),
                        std::io::ErrorKind::PermissionDenied => format!(
                            "Permission denied binding to port {} (network.expose host port).\n\
                             Remediation: use an unprivileged port or run with elevated privileges.",
                            mapping.host
                        ),
                        _ => format!(
                            "Cannot bind to port {} (network.expose host port): {}\n\
                             Remediation: check network configuration or update [network].expose host value in devpod.toml.",
                            mapping.host, e
                        ),
                    };
                    return Err(DevpodError::Command(detail));
                }
            }
        }

        // Check if cluster exists
        let check = Command::new("k3d")
            .args(["cluster", "list", &self.name])
            .output()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to check k3d cluster: {}", e)))?;

        if !check.status.success() {
            // Create cluster
            let mut args = vec![
                "cluster".to_string(),
                "create".to_string(),
                self.name.clone(),
                "--wait".to_string(),
            ];

            // Configure registry if enabled
            if config.registry.enabled {
                let registry_name = format!("{}-registry", self.name);
                args.push("--registry-create".to_string());
                // Map local port to registry container
                args.push(format!(
                    "{}:0.0.0.0:{}",
                    registry_name, config.registry.port
                ));
            }

            // Map ports from network config
            for mapping in &config.network.expose {
                args.push("-p".to_string());
                args.push(format!(
                    "{}:{}@loadbalancer",
                    mapping.host, mapping.container
                ));
            }

            // Map data volume if persistent storage is enabled
            if config.infrastructure.persistent_storage_enabled {
                let path_str = &config.infrastructure.data_mount_path;
                // Map local directory ./data/storage to the configured path
                let local_path = std::env::current_dir()?.join("data").join("storage");
                std::fs::create_dir_all(&local_path)?;

                args.push("--volume".to_string());
                args.push(format!("{}:{}@server:0", local_path.display(), path_str));
            }

            let status = Command::new("k3d")
                .args(&args)
                .status()
                .await
                .map_err(|e| {
                    DevpodError::Command(format!("Failed to create k3d cluster: {}", e))
                })?;

            if !status.success() {
                return Err(DevpodError::Command(
                    "k3d cluster creation failed (orchestrator=k3d).\n\
                     Common cause: host port conflict in [network].expose. Run `devpod up` again after fixing conflicts."
                        .into(),
                ));
            }
            info!("Cluster created");
        } else {
            // Ensure it's started
            let _ = Command::new("k3d")
                .args(["cluster", "start", &self.name])
                .status()
                .await;
            info!("Cluster started");
        }

        // Fetch and Merge kubeconfig natively without kubectl
        let k3d_kubeconfig_out = Command::new("k3d")
            .args(["kubeconfig", "get", &self.name])
            .output()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to get k3d kubeconfig: {}", e)))?;

        if !k3d_kubeconfig_out.status.success() {
            return Err(DevpodError::Command(
                "Failed to fetch k3d kubeconfig".into(),
            ));
        }

        let k3d_kubeconfig_str = String::from_utf8_lossy(&k3d_kubeconfig_out.stdout);
        let incoming: crate::util::kubeconfig::Kubeconfig =
            serde_yaml::from_str(&k3d_kubeconfig_str).map_err(|e| {
                DevpodError::KubeconfigMerge(format!("Failed to parse k3d kubeconfig: {}", e))
            })?;

        let home = dirs::home_dir().ok_or_else(|| {
            DevpodError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No home dir",
            ))
        })?;
        let default_kubeconfig = home.join(".kube").join("config");

        std::fs::create_dir_all(default_kubeconfig.parent().unwrap())?;

        let mut base_config = if default_kubeconfig.exists() {
            let content = std::fs::read_to_string(&default_kubeconfig)?;
            serde_yaml::from_str(&content).unwrap_or_default()
        } else {
            crate::util::kubeconfig::Kubeconfig::default()
        };

        crate::util::kubeconfig::merge_kubeconfig(&mut base_config, incoming);

        let merged_yaml = serde_yaml::to_string(&base_config)?;
        std::fs::write(&default_kubeconfig, merged_yaml)?;

        info!("Kubeconfig merged");

        Ok(())
    }

    async fn down(&self, _config: &DevpodConfig) -> Result<()> {
        let status = Command::new("k3d")
            .args(["cluster", "delete", &self.name])
            .status()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to delete k3d cluster: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to delete k3d cluster".into()));
        }
        Ok(())
    }

    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()> {
        if images.is_empty() {
            return Ok(());
        }

        info!("Importing images into k3d...");

        let mut args = vec!["image".to_string(), "import".to_string()];
        for img in images {
            args.push(img.to_string_lossy().to_string());
        }
        args.push("-c".to_string());
        args.push(self.name.clone());

        let status = Command::new("k3d")
            .args(&args)
            .status()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to import images to k3d: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to import images".into()));
        }
        info!("Images imported");
        Ok(())
    }

    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()> {
        // Patch manifests for local registry access
        info!("Patching manifests for k3d registry access...");
        let port_str = self.registry_port.to_string();
        let target = format!("localhost:{}", port_str);
        let replacement = format!("host.k3d.internal:{}", port_str);

        for entry in WalkDir::new(&yaml_path).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "yaml" || ext == "yml" {
                        let content = std::fs::read_to_string(path).unwrap_or_default();
                        if content.contains(&target) {
                            info!("Patching {}", path.display());
                            let new_content = content.replace(&target, &replacement);
                            if let Err(e) = std::fs::write(path, new_content) {
                                tracing::warn!("Failed to patch {}: {}", path.display(), e);
                            }
                        }
                    }
                }
            }
        }

        info!("Applying manifests from {}...", yaml_path.display());

        let output = Command::new("kubectl")
            .args(["apply", "-f", yaml_path.to_str().unwrap(), "--recursive"])
            .output()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to apply manifests: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevpodError::Command(format!(
                "kubectl apply failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            )));
        }

        info!("Manifests applied");
        Ok(())
    }

    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()> {
        if !config.secrets.enabled {
            return Ok(());
        }

        let secret_set = config.secrets.set.as_deref().unwrap_or("default");
        let context = format!("k3d-{}", self.name);

        info!(
            "Syncing secrets '{}' to context '{}'...",
            secret_set, context
        );

        let status = Command::new("ksecret")
            .arg("sync")
            .arg(secret_set)
            .arg("-c")
            .arg(&context)
            .arg("-n")
            .arg(&config.secrets.namespace)
            .status()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to run ksecret sync: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to sync secrets".into()));
        }

        info!("Secrets synced");
        Ok(())
    }
}
