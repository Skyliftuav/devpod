use crate::config::DevpodConfig;
use crate::error::{DevpodError, Result};
use crate::orchestrator::ClusterManager;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{info, warn};

pub struct K3sManager {
    name: String,
    bin_path: PathBuf,
}

impl K3sManager {
    pub fn new(name: &str) -> Self {
        let home = dirs::home_dir().unwrap_or(PathBuf::from("/root"));
        let bin_path = home.join(".devpod").join("bin").join("k3s");
        Self {
            name: name.to_string(),
            bin_path,
        }
    }

    async fn ensure_binary(&self) -> Result<()> {
        if self.bin_path.exists() {
            return Ok(());
        }

        info!("Downloading k3s binary...");
        std::fs::create_dir_all(self.bin_path.parent().unwrap())?;

        // Download binary (x86_64 statically linked)
        let url = "https://github.com/k3s-io/k3s/releases/download/v1.30.0%2Bk3s1/k3s";
        let resp = reqwest::get(url)
            .await
            .map_err(|e| DevpodError::Network(format!("Failed to download k3s: {}", e)))?
            .bytes()
            .await
            .map_err(|e| {
                DevpodError::Network(format!("Failed to read k3s binary stream: {}", e))
            })?;
        std::fs::write(&self.bin_path, resp)?;

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&self.bin_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&self.bin_path, perms)?;
        }

        Ok(())
    }
}

#[async_trait]
impl ClusterManager for K3sManager {
    async fn up(&self, _config: &DevpodConfig) -> Result<()> {
        self.ensure_binary().await?;

        info!("Starting native k3s instance for '{}'...", self.name);

        // Start k3s process in the background
        let args = [
            "server".to_string(),
            "--write-kubeconfig-mode".to_string(),
            "644".to_string(),
        ];

        info!("Executing: {} {}", self.bin_path.display(), args.join(" "));

        // Warn the user to ensure k3s is running via systemd if applicable
        warn!("Please ensure k3s is running via systemd or manually.");

        Ok(())
    }

    async fn down(&self, _config: &DevpodConfig) -> Result<()> {
        info!("Stopping k3s...");
        // Stop k3s via systemctl or by killing the process
        warn!("Please stop k3s manually (e.g. systemctl stop k3s).");
        Ok(())
    }

    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()> {
        // Move to k3s images dir
        let images_dir = PathBuf::from("/var/lib/rancher/k3s/agent/images/");
        std::fs::create_dir_all(&images_dir).ok(); // Might fail if not root/permissions

        for img in images {
            let target = images_dir.join(img.file_name().unwrap());
            info!("Importing image to {}", target.display());

            // This copy might require root permissions
            if let Err(e) = std::fs::copy(&img, &target) {
                warn!("Failed to copy image (permissions?): {}", e);
                info!(
                    "You might need to run: sudo cp {} {}",
                    img.display(),
                    target.display()
                );
            }
        }
        Ok(())
    }

    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()> {
        // Assumes KUBECONFIG is set or uses default ~/.kube/config
        info!("Applying manifests from {}...", yaml_path.display());

        let status = Command::new("kubectl")
            .args(["apply", "-f", yaml_path.to_str().unwrap(), "--recursive"])
            .status()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to apply manifests: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to apply manifests".into()));
        }

        info!("Manifests applied");
        Ok(())
    }

    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()> {
        if !config.secrets.enabled {
            return Ok(());
        }

        let secret_set = config.secrets.set.as_deref().unwrap_or("default");

        info!("Syncing secrets '{}' to k3s...", secret_set);

        let status = Command::new("ksecret")
            .arg("sync")
            .arg(secret_set)
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
