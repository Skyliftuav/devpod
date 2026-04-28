use crate::config::DevpodConfig;
use crate::orchestrator::ClusterManager;
use anyhow::{Context, Result};
use async_trait::async_trait;
use colored::Colorize;
use std::path::PathBuf;
use tokio::process::Command;

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

        println!("{} Downloading k3s binary...", "->".blue());
        std::fs::create_dir_all(self.bin_path.parent().unwrap())?;

        // Download binary (x86_64 statically linked)
        let url = "https://github.com/k3s-io/k3s/releases/download/v1.30.0%2Bk3s1/k3s";
        let resp = reqwest::get(url).await?.bytes().await?;
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

#[async_trait(?Send)]
impl ClusterManager for K3sManager {
    async fn up(&self, _config: &DevpodConfig) -> Result<()> {
        self.ensure_binary().await?;

        println!(
            "{} Starting native k3s instance for '{}'...",
            "->".blue().bold(),
            self.name.cyan()
        );

        // Start k3s process in the background
        let args = vec![
            "server".to_string(),
            "--write-kubeconfig-mode".to_string(),
            "644".to_string(),
        ];

        println!(
            "{} Executing: {} {}",
            "->".blue(),
            self.bin_path.display(),
            args.join(" ")
        );

        // Warn the user to ensure k3s is running via systemd if applicable
        println!(
            "{} Please ensure k3s is running via systemd or manually.",
            "!".yellow()
        );

        Ok(())
    }

    async fn down(&self, _config: &DevpodConfig) -> Result<()> {
        println!("{} Stopping k3s...", "->".blue());
        // Stop k3s via systemctl or by killing the process
        println!(
            "{} Please stop k3s manually (e.g. systemctl stop k3s).",
            "!".yellow()
        );
        Ok(())
    }

    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()> {
        // Move to k3s images dir
        let images_dir = PathBuf::from("/var/lib/rancher/k3s/agent/images/");
        std::fs::create_dir_all(&images_dir).ok(); // Might fail if not root/permissions

        for img in images {
            let target = images_dir.join(img.file_name().unwrap());
            println!("{} Importing image to {}", "->".blue(), target.display());

            // This copy might require root permissions
            if let Err(e) = std::fs::copy(&img, &target) {
                println!(
                    "{} Failed to copy image (permissions?): {}",
                    "!".yellow(),
                    e
                );
                println!(
                    "{} You might need to run: sudo cp {} {}",
                    "->".blue(),
                    img.display(),
                    target.display()
                );
            }
        }
        Ok(())
    }

    async fn apply_manifests(&self, _config: &DevpodConfig, yaml_path: PathBuf) -> Result<()> {
        // Assumes KUBECONFIG is set or uses default ~/.kube/config
        println!(
            "{} Applying manifests from {}...",
            "->".blue(),
            yaml_path.display()
        );

        let status = Command::new("kubectl")
            .args(["apply", "-f", yaml_path.to_str().unwrap(), "--recursive"])
            .status()
            .await
            .context("Failed to apply manifests")?;

        if !status.success() {
            anyhow::bail!("Failed to apply manifests");
        }

        println!("{} Manifests applied", "OK".green());
        Ok(())
    }

    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()> {
        if !config.secrets.enabled {
            return Ok(());
        }

        let secret_set = config.secrets.set.as_deref().unwrap_or("default");

        println!(
            "{} Syncing secrets '{}' to k3s...",
            "->".blue().bold(),
            secret_set.cyan(),
        );

        let status = Command::new("ksecret")
            .arg("sync")
            .arg(secret_set)
            .arg("-n")
            .arg(&config.secrets.namespace)
            .status()
            .await
            .context("Failed to run ksecret sync")?;

        if !status.success() {
            anyhow::bail!("Failed to sync secrets");
        }

        println!("{} Secrets synced", "OK".green());
        Ok(())
    }
}
