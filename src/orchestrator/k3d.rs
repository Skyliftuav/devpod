use crate::config::DevpodConfig;
use crate::orchestrator::ClusterManager;
use anyhow::{Context, Result};
use async_trait::async_trait;
use colored::Colorize;
use std::path::PathBuf;
use tokio::process::Command;

pub struct K3dManager {
    name: String,
}

impl K3dManager {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[async_trait]
impl ClusterManager for K3dManager {
    async fn up(&self, config: &DevpodConfig) -> Result<()> {
        println!(
            "{} Provisioning k3d cluster '{}'...",
            "->".blue().bold(),
            self.name.cyan()
        );

        // Check if Docker is running
        let docker_check = Command::new("docker")
            .arg("info")
            .output()
            .await
            .context("Failed to run docker info. Is Docker installed?")?;
        
        if !docker_check.status.success() {
            anyhow::bail!("Docker is not running. Please start Docker Desktop/OrbStack.");
        }

        // Check if cluster exists
        let check = Command::new("k3d")
            .args(["cluster", "list", &self.name])
            .output()
            .await
            .context("Failed to check k3d cluster")?;

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
                args.push(format!("{}:0.0.0.0:{}", registry_name, config.registry.port));
            }

            // Map ports from network config
            for mapping in &config.network.expose {
                args.push("-p".to_string());
                args.push(format!("{}:{}@loadbalancer", mapping.host, mapping.container));
            }
            
            // Map data volume if persistent storage is enabled
            if config.infrastructure.persistent_storage_enabled {
                 let path_str = &config.infrastructure.data_mount_path;
                 // In k3d (docker), we map a local volume to the node path
                 // For simplicity, we'll map a local dir ./data/storage to the configured path
                 let local_path = std::env::current_dir()?.join("data").join("storage");
                 std::fs::create_dir_all(&local_path)?;
                 
                 args.push("--volume".to_string());
                 args.push(format!("{}:{}@server:0", local_path.display(), path_str));
            }

            let status = Command::new("k3d")
                .args(&args)
                .status()
                .await
                .context("Failed to create k3d cluster")?;

            if !status.success() {
                anyhow::bail!("k3d cluster creation failed");
            }
            println!("{} Cluster created", "OK".green());
        } else {
             // Ensure it's started
             let _ = Command::new("k3d")
                .args(["cluster", "start", &self.name])
                .status()
                .await;
             println!("{} Cluster started", "OK".green());
        }
        
        // Merge kubeconfig
        let _ = Command::new("k3d")
            .args(["kubeconfig", "merge", &self.name, "--kubeconfig-merge-default"])
            .status()
            .await;

        Ok(())
    }

    async fn down(&self) -> Result<()> {
         let status = Command::new("k3d")
            .args(["cluster", "delete", &self.name])
            .status()
            .await
            .context("Failed to delete k3d cluster")?;

        if !status.success() {
            anyhow::bail!("Failed to delete k3d cluster");
        }
        Ok(())
    }

    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()> {
        if images.is_empty() {
            return Ok(());
        }

        println!("{} Importing images into k3d...", "->".blue());
        
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
            .context("Failed to import images to k3d")?;

        if !status.success() {
            anyhow::bail!("Failed to import images");
        }
        println!("{} Images imported", "OK".green());
        Ok(())
    }

    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()> {
         println!("{} Applying manifests from {}...", "->".blue(), yaml_path.display());
         
         let status = Command::new("kubectl")
            .args(["apply", "-f", yaml_path.to_str().unwrap()])
            .status()
            .await
            .context("Failed to apply manifests")?;
        
        if !status.success() {
            anyhow::bail!("Failed to apply manifests");
        }
        
        println!("{} Manifests applied", "OK".green());
        Ok(())
    }
}
