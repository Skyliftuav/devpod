use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::Command;

pub struct RemoteExecutor;

impl RemoteExecutor {
    pub async fn execute(host: &str, user: &str, command: &str) -> Result<String> {
        let target = format!("{}@{}", user, host);
        let status = Command::new("ssh")
            .arg(&target)
            .arg(command)
            .output()
            .await
            .context(format!("Failed to execute command on {}", target))?;

        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            anyhow::bail!("Remote command failed: {}", stderr.trim());
        }

        Ok(String::from_utf8_lossy(&status.stdout).to_string())
    }
    
    // For interactive shell
    pub async fn shell(host: &str, user: &str) -> Result<()> {
        let target = format!("{}@{}", user, host);
        let status = Command::new("ssh")
            .arg(&target)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
            
        if !status.success() {
            anyhow::bail!("SSH session exited with error");
        }
        Ok(())
    }

    pub async fn scp_from(host: &str, user: &str, remote_path: &str, local_path: &str) -> Result<()> {
        let source = format!("{}@{}:{}", user, host, remote_path);
        let status = Command::new("scp")
            .arg(&source)
            .arg(local_path)
            .status()
            .await
            .context("Failed to SCP file")?;

        if !status.success() {
            anyhow::bail!("SCP failed");
        }
        Ok(())
    }
}
