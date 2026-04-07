use crate::config::DevpodConfig;
use crate::error::{DevpodError, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{info, instrument, warn};

pub struct Builder;

impl Builder {
    #[instrument(skip(config), fields(tool = %config.deployment.tool, env = %config.deployment.environment))]
    pub async fn build(config: &DevpodConfig) -> Result<()> {
        info!("Building services via {}...", config.deployment.tool);

        let tool = &config.deployment.tool;
        if tool == "sailr" {
            // Run sailr build
            let status = Command::new("sailr")
                .arg("build")
                .arg("--name")
                .arg(&config.deployment.environment)
                .status()
                .await
                .map_err(|e| DevpodError::Command(format!("Failed to run sailr build: {}", e)))?;

            if !status.success() {
                return Err(DevpodError::Command("sailr build failed".into()));
            }

            // Run sailr generate
            let status = Command::new("sailr")
                .arg("generate")
                .arg("--name")
                .arg(&config.deployment.environment)
                .status()
                .await
                .map_err(|e| {
                    DevpodError::Command(format!("Failed to run sailr generate: {}", e))
                })?;

            if !status.success() {
                return Err(DevpodError::Command("sailr generate failed".into()));
            }
        } else {
            warn!("Unknown build tool '{}', skipping build step", tool);
        }

        Ok(())
    }

    pub fn get_manifest_path(config: &DevpodConfig) -> PathBuf {
        // Assumption: sailr outputs to ./k8s/generated/<env>/
        Path::new("k8s")
            .join("generated")
            .join(&config.deployment.environment)
    }

    // Helper to identify built images
    #[allow(dead_code)]
    pub async fn export_images(_config: &DevpodConfig) -> Result<Vec<PathBuf>> {
        // Placeholder returning empty list
        Ok(vec![])
    }
}
