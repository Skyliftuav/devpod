use crate::error::{DevpodError, Result};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(&self, host: &str, user: &str, command: &str) -> Result<String>;
    async fn shell(&self, host: &str, user: &str) -> Result<()>;
    async fn scp_from(
        &self,
        host: &str,
        user: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<()>;
}

pub struct RemoteExecutor;

#[async_trait]
impl Executor for RemoteExecutor {
    async fn execute(&self, host: &str, user: &str, command: &str) -> Result<String> {
        let target = format!("{}@{}", user, host);
        let status = Command::new("ssh")
            .arg(&target)
            .arg(command)
            .output()
            .await
            .map_err(|e| DevpodError::Execution {
                host: target.clone(),
                msg: format!("Failed to execute ssh: {}", e),
            })?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            return Err(DevpodError::Execution {
                host: target,
                msg: format!("Remote command failed: {}", stderr.trim()),
            });
        }

        Ok(String::from_utf8_lossy(&status.stdout).to_string())
    }

    // For interactive shell
    async fn shell(&self, host: &str, user: &str) -> Result<()> {
        let target = format!("{}@{}", user, host);
        let status = Command::new("ssh")
            .arg(&target)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await
            .map_err(|e| DevpodError::Execution {
                host: target.clone(),
                msg: format!("Failed to start ssh shell: {}", e),
            })?;

        if !status.success() {
            return Err(DevpodError::Execution {
                host: target,
                msg: "SSH session exited with error".to_string(),
            });
        }
        Ok(())
    }

    async fn scp_from(
        &self,
        host: &str,
        user: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<()> {
        let source = format!("{}@{}:{}", user, host, remote_path);
        let status = Command::new("scp")
            .arg(&source)
            .arg(local_path)
            .status()
            .await
            .map_err(|e| DevpodError::Execution {
                host: host.to_string(),
                msg: format!("Failed to SCP file: {}", e),
            })?;

        if !status.success() {
            return Err(DevpodError::Execution {
                host: host.to_string(),
                msg: "SCP failed".to_string(),
            });
        }
        Ok(())
    }
}
