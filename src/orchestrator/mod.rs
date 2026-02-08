use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

pub mod k3d;
pub mod k3s;

use crate::config::DevpodConfig;

#[async_trait]
pub trait ClusterManager: Send + Sync {
    /// Provision and start the cluster
    async fn up(&self, config: &DevpodConfig) -> Result<()>;
    
    /// Teardown the cluster
    async fn down(&self) -> Result<()>;
    
    /// Load container images into the cluster
    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()>;
    
    /// Apply Kubernetes manifests
    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()>;
}

pub fn get_manager(config: &DevpodConfig) -> Box<dyn ClusterManager> {
    // If provider type is explicit, use it. Otherwise auto-detect.
    let provider = if config.provider.provider_type == "auto" {
        if cfg!(target_os = "linux") {
            "k3s"
        } else {
            "k3d"
        }
    } else {
        config.provider.provider_type.as_str()
    };

    match provider {
        "k3s" => Box::new(k3s::K3sManager::new(&config.project.name)),
        "k3d" | _ => Box::new(k3d::K3dManager::new(&config.project.name)),
    }
}
