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
        // But /etc/rancher/k3s/k3s.yaml is root owned. We need to copy it to a temp file readable by user first.
        let remote_temp = "/tmp/k3s.yaml";

        // 1. Copy to temp and chmod
        RemoteExecutor::execute(
            server_host,
            user,
            &format!(
                "sudo cp /etc/rancher/k3s/k3s.yaml {} && sudo chmod 644 {}",
                remote_temp, remote_temp
            ),
        )
        .await?;

        // 2. SCP
        if let Err(e) =
            RemoteExecutor::scp_from(server_host, user, remote_temp, temp_path_str).await
        {
            // Cleanup even if fail
            let _ = RemoteExecutor::execute(server_host, user, &format!("sudo rm {}", remote_temp))
                .await;
            return Err(e);
        }

        // 3. Cleanup remote temp
        let _ =
            RemoteExecutor::execute(server_host, user, &format!("sudo rm {}", remote_temp)).await;

        // Read and patch
        let content = std::fs::read_to_string(&temp_kubeconfig)?;

        let context_name = format!("devpod-{}", self.env_name);

        // Use clean replacement chain on original content
        // 1. Replace localhost/127.0.0.1 with correct IP
        // 2. Rename cluster, user, and context definitions (name: default)
        // 3. Rename references in context (cluster: default, user: default)
        // 4. Update current-context
        let patched_content = content
            .replace("127.0.0.1", server_host)
            .replace("localhost", server_host)
            .replace("name: default", &format!("name: {}", context_name))
            .replace("cluster: default", &format!("cluster: {}", context_name))
            .replace("user: default", &format!("user: {}", context_name))
            .replace(
                "current-context: default",
                &format!("current-context: {}", context_name),
            );

        let patched_path = temp_dir.path().join("patched_k3s.yaml");
        std::fs::write(&patched_path, patched_content.clone())?;

        // We don't need to rename-context via kubectl anymore since we patched the file content directly.

        println!(
            "{} Merging kubeconfig into default context '{}'",
            "->".blue(),
            context_name
        );

        // This is a bit risky to overwrite user config automatically without backup,
        // but it's what was requested.
        let home = dirs::home_dir().context("No home dir")?;
        let default_kubeconfig = home.join(".kube").join("config");

        // Ensure .kube dir exists
        std::fs::create_dir_all(default_kubeconfig.parent().unwrap())?;

        // Read and parse patched kubeconfig
        let incoming: crate::util::kubeconfig::Kubeconfig =
            serde_yaml::from_str(&patched_content).context("Failed to parse patched kubeconfig")?;

        let mut base_config = if default_kubeconfig.exists() {
            let content = std::fs::read_to_string(&default_kubeconfig)?;
            serde_yaml::from_str(&content).unwrap_or_default()
        } else {
            crate::util::kubeconfig::Kubeconfig::default()
        };

        crate::util::kubeconfig::merge_kubeconfig(&mut base_config, incoming);

        let merged_yaml = serde_yaml::to_string(&base_config)?;
        std::fs::write(&default_kubeconfig, merged_yaml)?;

        println!("{} Kubeconfig merged", "OK".green());
        Ok(())
    }
    fn generate_token() -> String {
        use std::process::Command as StdCommand;
        // Try openssl first, fall back to /dev/urandom
        if let Ok(output) = StdCommand::new("openssl")
            .args(["rand", "-hex", "32"])
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }
        // Fallback: read from /dev/urandom
        if let Ok(output) = StdCommand::new("sh")
            .arg("-c")
            .arg("head -c 32 /dev/urandom | xxd -p -c 64")
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }
        // Last resort: timestamp-based (not ideal but functional)
        format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
    async fn wait_for_api_server(&self, host: &str, user: &str) -> Result<()> {
        let max_retries = 30;
        for i in 0..max_retries {
            match RemoteExecutor::execute(host, user, "sudo k3s kubectl get nodes").await {
                Ok(_) => {
                    println!("   {} API server ready on {}", "OK".green(), host);
                    return Ok(());
                }
                Err(_) => {
                    if i >= max_retries - 1 {
                        anyhow::bail!("Timed out waiting for API server on {}", host);
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ClusterManager for RemoteManager {
    async fn up(&self, config: &DevpodConfig) -> Result<()> {
        let cluster = config.get_cluster(&self.env_name).context(format!(
            "Cluster definition for '{}' not found in config",
            self.env_name
        ))?;

        if cluster.provider != "k3s" {
            anyhow::bail!("Remote provider must be 'k3s'");
        }

        let user = cluster.user.as_deref().unwrap_or("root");

        // 1. Identify Server vs Agents
        let servers: Vec<_> = cluster
            .nodes
            .iter()
            .filter(|n| n.role == "server")
            .collect();
        let agents: Vec<_> = cluster.nodes.iter().filter(|n| n.role == "agent").collect();

        if servers.is_empty() {
            anyhow::bail!("No server node defined for remote cluster");
        }

        let first_server_ip = servers[0].address.clone();

        // 2. Determine Cluster Token
        // Check if token already exists on the first server (idempotency)
        let token = match RemoteExecutor::execute(
            &first_server_ip,
            user,
            "sudo cat /var/lib/rancher/k3s/server/node-token",
        )
        .await
        {
            Ok(t) => {
                println!(
                    "{} Reusing existing cluster token from {}",
                    "OK".green(),
                    first_server_ip
                );
                t.trim().to_string()
            }
            Err(_) => {
                let t = Self::generate_token();
                println!("{} Generated new cluster token", "OK".green());
                t
            }
        };

        // Helper closure to build the runtime flag
        let runtime_flag = |runtime: &str| -> &str {
            if runtime == "docker" {
                "--docker"
            } else {
                ""
            }
        };

        let datastore = cluster.datastore_endpoint.as_deref();

        if let Some(ds_endpoint) = datastore {
            println!(
                "{} Provisioning HA Cluster with External Datastore...",
                "->".blue().bold()
            );

            for (i, server) in servers.iter().enumerate() {
                println!(
                    "{} Provisioning Server Node {}: {}",
                    "->".blue(),
                    i,
                    server.address
                );

                let cmd = format!(
                    "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --datastore-endpoint=\"{}\" --tls-san {}",
                    token, runtime_flag(&server.runtime), ds_endpoint, server.address
                );

                match RemoteExecutor::execute(&server.address, user, &cmd).await {
                    Ok(_) => println!(
                        "   {} K3s server installed on {}",
                        "OK".green(),
                        server.address
                    ),
                    Err(e) => {
                        println!(
                            "{} K3s installation failed on {}. Fetching logs...",
                            "!".red(),
                            server.address
                        );
                        let logs = RemoteExecutor::execute(
                            &server.address,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                        return Err(e);
                    }
                }
            }
        } else if servers.len() > 1 {
            println!(
                "{} Provisioning HA Cluster with Embedded Etcd...",
                "->".blue().bold()
            );

            // Server 0: --cluster-init
            let primary = servers[0];
            println!(
                "{} Initializing Cluster on Primary: {}",
                "->".blue(),
                primary.address
            );

            let cmd_init = format!(
                "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --cluster-init --tls-san {}",
                token, runtime_flag(&primary.runtime), primary.address
            );

            match RemoteExecutor::execute(&primary.address, user, &cmd_init).await {
                Ok(_) => println!("   {} Primary initialized", "OK".green()),
                Err(e) => {
                    println!(
                        "{} K3s initialization failed on {}. Fetching logs...",
                        "!".red(),
                        primary.address
                    );
                    let logs = RemoteExecutor::execute(
                        &primary.address,
                        user,
                        "sudo journalctl -xeu k3s.service --no-pager -n 50",
                    )
                    .await
                    .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                    return Err(e);
                }
            }

            // Wait for API server to be ready before joining other servers
            println!(
                "{} Waiting for API server on {}...",
                "->".blue(),
                primary.address
            );
            self.wait_for_api_server(&primary.address, user).await?;

            // Other Servers join
            for server in servers.iter().skip(1) {
                println!("{} Joining Server: {}", "->".blue(), server.address);
                let cmd_join = format!(
                    "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --server https://{}:6443 --tls-san {}",
                    token, runtime_flag(&server.runtime), first_server_ip, server.address
                );

                match RemoteExecutor::execute(&server.address, user, &cmd_join).await {
                    Ok(_) => println!("   {} Server join complete", "OK".green()),
                    Err(e) => {
                        println!(
                            "{} K3s join failed on {}. Fetching logs...",
                            "!".red(),
                            server.address
                        );
                        let logs = RemoteExecutor::execute(
                            &server.address,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                        return Err(e);
                    }
                }
            }
        } else {
            // Single Server
            let primary = servers[0];
            println!(
                "{} Provisioning Single-Node Server: {}",
                "->".blue().bold(),
                primary.address
            );

            let install_cmd = format!(
                "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --tls-san {}",
                token,
                runtime_flag(&primary.runtime),
                primary.address
            );

            match RemoteExecutor::execute(&primary.address, user, &install_cmd).await {
                Ok(_) => println!(
                    "   {} K3s server installed on {}",
                    "OK".green(),
                    primary.address
                ),
                Err(e) => {
                    println!(
                        "{} K3s installation failed on {}. Fetching logs...",
                        "!".red(),
                        primary.address
                    );
                    let logs = RemoteExecutor::execute(
                        &primary.address,
                        user,
                        "sudo journalctl -xeu k3s.service --no-pager -n 50",
                    )
                    .await
                    .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                    return Err(e);
                }
            }
        }

        // 4. Join Agents (Common for all modes)
        if !agents.is_empty() {
            // Wait for API server to be ready before joining agents
            println!(
                "{} Waiting for API server on {}...",
                "->".blue(),
                first_server_ip
            );
            self.wait_for_api_server(&first_server_ip, user).await?;

            println!("{} Joining Agents...", "->".blue());
            for agent in agents {
                println!("{} Joining Agent: {}", "->".blue(), agent.address);
                let join_cmd = format!(
                    "curl -sfL https://get.k3s.io | K3S_URL=https://{}:6443 K3S_TOKEN={} sh -s - agent {}",
                    first_server_ip, token, runtime_flag(&agent.runtime)
                );

                RemoteExecutor::execute(&agent.address, user, &join_cmd).await?;
                println!("   {} Agent joined", "OK".green());
            }
        }

        // 5. Fetch Kubeconfig
        self.fetch_and_merge_kubeconfig(&first_server_ip, user)
            .await?;

        Ok(())
    }

    async fn down(&self, config: &DevpodConfig) -> Result<()> {
        let cluster = config.get_cluster(&self.env_name).context(format!(
            "Cluster definition for '{}' not found in config",
            self.env_name
        ))?;

        if cluster.provider != "k3s" {
            anyhow::bail!("Remote provider must be 'k3s'");
        }

        let user = cluster.user.as_deref().unwrap_or("root");

        // 1. Identify Server vs Agents
        let servers: Vec<_> = cluster
            .nodes
            .iter()
            .filter(|n| n.role == "server")
            .collect();
        let agents: Vec<_> = cluster.nodes.iter().filter(|n| n.role == "agent").collect();

        // 2. Uninstall Agents first
        for agent in agents {
            println!(
                "{} Uninstalling K3s Agent on {}...",
                "->".blue(),
                agent.address
            );
            // Ignore errors if already uninstalled or unreachable?
            // "k3s-agent-uninstall.sh" usually available in path
            let _ = RemoteExecutor::execute(
                &agent.address,
                user,
                "/usr/local/bin/k3s-agent-uninstall.sh",
            )
            .await;

            // Clean up data
            let _ = RemoteExecutor::execute(
                &agent.address,
                user,
                "sudo rm -rf /var/lib/rancher/k3s /etc/rancher/k3s",
            )
            .await;
            println!("   {} Agent uninstalled & data purged", "OK".green());
        }

        // 3. Uninstall Servers
        for server in servers {
            println!(
                "{} Uninstalling K3s Server on {}...",
                "->".blue(),
                server.address
            );
            let _ =
                RemoteExecutor::execute(&server.address, user, "/usr/local/bin/k3s-uninstall.sh")
                    .await;

            // Clean up data
            let _ = RemoteExecutor::execute(
                &server.address,
                user,
                "sudo rm -rf /var/lib/rancher/k3s /etc/rancher/k3s",
            )
            .await;
            println!("   {} Server uninstalled & data purged", "OK".green());
        }

        // 4. Cleanup Local Kubeconfig
        let context_name = format!("devpod-{}", self.env_name);
        println!(
            "{} Cleaning up local context '{}'...",
            "->".blue(),
            context_name
        );

        let _ = Command::new("kubectl")
            .args(["config", "delete-context", &context_name])
            .output()
            .await;

        let _ = Command::new("kubectl")
            .args(["config", "delete-cluster", &context_name])
            .output()
            .await;

        let _ = Command::new("kubectl")
            .args(["config", "delete-user", &context_name])
            .output()
            .await;

        println!("{} Local cleanup complete", "OK".green());

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
