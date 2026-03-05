use crate::config::DevpodConfig;
use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub struct Builder;

impl Builder {
    pub async fn build(config: &DevpodConfig) -> Result<()> {
        println!(
            "{} Building services via {}...",
            "->".blue().bold(),
            config.deployment.tool
        );

        let tool = &config.deployment.tool;
        if tool == "sailr" {
            // Run sailr build
            let status = Command::new("sailr")
                .arg("build")
                .arg("--name")
                .arg(&config.deployment.environment)
                .status()
                .await
                .context("Failed to run sailr build")?;

            if !status.success() {
                anyhow::bail!("sailr build failed");
            }

            // Run sailr generate
            let status = Command::new("sailr")
                .arg("generate")
                .arg("--name")
                .arg(&config.deployment.environment)
                .status()
                .await
                .context("Failed to run sailr generate")?;

            if !status.success() {
                anyhow::bail!("sailr generate failed");
            }
        } else {
            println!(
                "{} Unknown build tool '{}', skipping build step",
                "!".yellow(),
                tool
            );
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
