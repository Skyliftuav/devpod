pub mod k3d;
pub mod k3s;
pub mod remote;

use crate::config::DevpodConfig;
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

#[async_trait(?Send)]
pub trait ClusterManager: Send + Sync {
    /// Provision and start the cluster
    async fn up(&self, config: &DevpodConfig) -> Result<()>;

    /// Teardown the cluster
    async fn down(&self, config: &DevpodConfig) -> Result<()>;

    /// Load container images into the cluster
    #[allow(dead_code)]
    async fn sync_images(&self, images: Vec<PathBuf>) -> Result<()>;

    /// Apply Kubernetes manifests
    async fn apply_manifests(&self, config: &DevpodConfig, yaml_path: PathBuf) -> Result<()>;

    /// Sync secrets via ksecret (or other tool)
    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()>;
}

pub fn get_manager(config: &DevpodConfig, env_name: Option<&str>) -> Box<dyn ClusterManager> {
    // If specific environment requested
    if let Some(env) = env_name {
        // Check if this environment is defined in the cluster map
        if config.get_cluster(env).is_some() {
            return Box::new(remote::RemoteManager::new(env));
        }
    }

    // Default to local detection if no env match or default env
    if cfg!(target_os = "linux") {
        Box::new(k3s::K3sManager::new(&config.project.name))
    } else {
        Box::new(k3d::K3dManager::new(
            &config.project.name,
            config.registry.port,
        ))
    }
}
