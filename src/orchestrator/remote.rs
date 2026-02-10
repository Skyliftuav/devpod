use crate::config::DevpodConfig;
use crate::executor::RemoteExecutor;
use crate::orchestrator::ClusterManager;
use anyhow::{Context, Result};
use async_trait::async_trait;
use colored::Colorize;
use std::path::PathBuf;
use tokio::process::Command;

pub struct RemoteManager {
    env_name: String,
}

impl RemoteManager {
    pub fn new(env_name: &str) -> Self {
        Self {
            env_name: env_name.to_string(),
        }
    }

    async fn fetch_and_merge_kubeconfig(&self, server_host: &str, user: &str) -> Result<()> {
        println!("{} Fetching kubeconfig...", "->".blue());
        
        let temp_dir = tempfile::tempdir()?;
        let temp_kubeconfig = temp_dir.path().join("k3s.yaml");
        let temp_path_str = temp_kubeconfig.to_str().unwrap();

        // SCP from remote
        RemoteExecutor::scp_from(server_host, user, "/etc/rancher/k3s/k3s.yaml", temp_path_str).await?;

        // Read and patch
        let content = std::fs::read_to_string(&temp_kubeconfig)?;
        // Replace localhost/127.0.0.1 with remote IP
        let patched_content = content.replace("127.0.0.1", server_host).replace("localhost", server_host);
        
        let context_name = format!("devpod-{}", self.env_name);
        
        // We need to merge this into ~/.kube/config
        // Safest way is to write patched file and use kubectl to merge?
        // Or manual merge.
        // Let's use kubectl config view --flatten approach:
        // 1. Write patched to file
        // 2. Set KUBECONFIG=~/.kube/config:new_file
        // 3. kubectl config view --flatten > merged_file
        // 4. Move merged_file to ~/.kube/config
        
        let patched_path = temp_dir.path().join("patched_k3s.yaml");
        std::fs::write(&patched_path, patched_content)?;

        // Rename context in patched file? 
        // kubectl config rename-context default <context_name> --kubeconfig ...
        let _ = Command::new("kubectl")
            .args(["config", "rename-context", "default", &context_name, "--kubeconfig", patched_path.to_str().unwrap()])
            .output()
            .await?;

        println!("{} Merging kubeconfig into default context '{}'", "->".blue(), context_name);
        
        // This is a bit risky to overwrite user config automatically without backup,
        // but it's what was requested.
        let home = dirs::home_dir().context("No home dir")?;
        let default_kubeconfig = home.join(".kube").join("config");
        
        // Ensure .kube dir exists
        std::fs::create_dir_all(default_kubeconfig.parent().unwrap())?;

        // Simple append strategy via env var if kubectl is used subsequently in the same shell
        // But to persist:
        if default_kubeconfig.exists() {
             let status = Command::new("kubectl")
                .env("KUBECONFIG", format!("{}:{}", default_kubeconfig.display(), patched_path.display()))
                .args(["config", "view", "--flatten"])
                .stdout(std::fs::File::create(&default_kubeconfig)?) // Overwrite!
                .status()
                .await?;
             
             if !status.success() {
                 anyhow::bail!("Failed to merge kubeconfig");
             }
        } else {
             std::fs::copy(&patched_path, &default_kubeconfig)?;
        }
        
        println!("{} Kubeconfig merged", "OK".green());
        Ok(())
    }
}

#[async_trait]
impl ClusterManager for RemoteManager {
    async fn up(&self, config: &DevpodConfig) -> Result<()> {
        let cluster = config.get_cluster(&self.env_name)
            .context(format!("Cluster definition for '{}' not found in config", self.env_name))?;

        if cluster.provider != "k3s" {
            anyhow::bail!("Remote provider must be 'k3s'");
        }

        let user = cluster.user.as_deref().unwrap_or("root");

        // 1. Identify Server vs Agents
        let servers: Vec<_> = cluster.nodes.iter().filter(|n| n.role == "server").collect();
        let agents: Vec<_> = cluster.nodes.iter().filter(|n| n.role == "agent").collect();

        if servers.is_empty() {
            anyhow::bail!("No server node defined for remote cluster");
        }

        let primary_server = servers[0];
        println!("{} Provisioning Primary Server: {}", "->".blue().bold(), primary_server.address);

        // 2. Install K3s on Primary Server
        let install_cmd = if primary_server.runtime == "docker" {
             "curl -sfL https://get.k3s.io | sh -s - server --docker" 
        } else {
             "curl -sfL https://get.k3s.io | sh -s - server"
        };
        
        // Append extra labels/args if needed
        // For now simple bootstrap
        
        RemoteExecutor::execute(&primary_server.address, user, install_cmd).await?;
        println!("   {} K3s server installed on {}", "OK".green(), primary_server.address);

        // 3. Get Token
        let token_output = RemoteExecutor::execute(
            &primary_server.address, 
            user, 
            "sudo cat /var/lib/rancher/k3s/server/node-token"
        ).await?;
        let token = token_output.trim();
        
        // 4. Join Agents
        for agent in agents {
             println!("{} Joining Agent: {}", "->".blue(), agent.address);
             let runtime_flag = if agent.runtime == "docker" { "--docker" } else { "" };
             let join_cmd = format!(
                 "curl -sfL https://get.k3s.io | K3S_URL=https://{}:6443 K3S_TOKEN={} sh -s - agent {}",
                 primary_server.address, token, runtime_flag
             );
             
             RemoteExecutor::execute(&agent.address, user, &join_cmd).await?;
             println!("   {} Agent joined", "OK".green());
        }

        // 5. Fetch Kubeconfig
        self.fetch_and_merge_kubeconfig(&primary_server.address, user).await?;

        Ok(())
    }

    async fn down(&self) -> Result<()> {
         println!("{} 'down' not implemented for remote clusters yet (run k3s-uninstall.sh manually)", "!".yellow());
         Ok(())
    }

    async fn sync_images(&self, _images: Vec<PathBuf>) -> Result<()> {
        // Placeholder for image sync logic (scp + ctr import)
        Ok(())
    }

    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()> {
         println!("{} Applying manifests to remote...", "->".blue());
         // Use the context we just created/merged
         let context_name = format!("devpod-{}", self.env_name);
         
         let status = Command::new("kubectl")
            .arg("--context")
            .arg(&context_name)
            .args(["apply", "-f", yaml_path.to_str().unwrap(), "--recursive"])
            .status()
            .await?;

        if !status.success() {
             anyhow::bail!("Failed to apply manifests");
        }
        Ok(())
    }

    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()> {
        if !config.secrets.enabled {
            return Ok(());
        }

        let secret_set = config.secrets.set.as_deref().unwrap_or("default");
        let context_name = format!("devpod-{}", self.env_name);

        println!(
            "{} Syncing secrets '{}' to remote context '{}'...",
            "->".blue().bold(),
            secret_set.cyan(),
            context_name
        );

        let status = Command::new("ksecret")
            .arg("sync")
            .arg(secret_set)
            .arg("-c")
            .arg(&context_name)
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
