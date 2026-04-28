use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{sleep, Duration};

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

    pub async fn can_connect(host: &str, user: &str) -> bool {
        let target = format!("{}@{}", user, host);
        for attempt in 0..5 {
            let result = Command::new("ssh")
                .args([
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "ConnectTimeout=5",
                    "-o",
                    "StrictHostKeyChecking=accept-new",
                ])
                .arg(&target)
                .arg("true")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;

            if let Ok(status) = result {
                if status.success() {
                    return true;
                }
            }

            if attempt < 4 {
                sleep(Duration::from_secs(2)).await;
            }
        }

        false
    }

    pub async fn first_reachable<'a, I>(hosts: I, user: &str) -> Option<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        for host in hosts {
            if Self::can_connect(host, user).await {
                return Some(host.to_string());
            }
        }

        None
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

    pub async fn scp_from(
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
            .context("Failed to SCP file")?;

        if !status.success() {
            anyhow::bail!("SCP failed");
        }
        Ok(())
    }
}
