use crate::config::DevpodConfig;
use crate::error::{DevpodError, Result};
use crate::executor::Executor;
use crate::orchestrator::ClusterManager;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::info;

pub struct RemoteManager {
    env_name: String,
    executor: Box<dyn Executor>,
}

impl RemoteManager {
    pub fn new(env_name: &str, executor: Box<dyn Executor>) -> Self {
        Self {
            env_name: env_name.to_string(),
            executor,
        }
    }

    async fn fetch_and_merge_kubeconfig(&self, server_host: &str, user: &str) -> Result<()> {
        info!("Fetching kubeconfig...");

        let temp_dir = tempfile::tempdir()?;
        let temp_kubeconfig = temp_dir.path().join("k3s.yaml");
        let temp_path_str = temp_kubeconfig.to_str().unwrap();

        // SCP from remote
        // But /etc/rancher/k3s/k3s.yaml is root owned. We need to copy it to a temp file readable by user first.
        let remote_temp = "/tmp/k3s.yaml";

        // 1. Copy to temp and chmod
        self.executor
            .execute(
                server_host,
                user,
                &format!(
                    "sudo cp /etc/rancher/k3s/k3s.yaml {} && sudo chmod 644 {}",
                    remote_temp, remote_temp
                ),
            )
            .await?;

        // 2. SCP
        if let Err(e) = self
            .executor
            .scp_from(server_host, user, remote_temp, temp_path_str)
            .await
        {
            // Cleanup even if fail
            let _ = self
                .executor
                .execute(server_host, user, &format!("sudo rm {}", remote_temp))
                .await;
            return Err(e);
        }

        // 3. Cleanup remote temp
        let _ = self
            .executor
            .execute(server_host, user, &format!("sudo rm {}", remote_temp))
            .await;

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

        info!("Merging kubeconfig into default context '{}'", context_name);

        // This is a bit risky to overwrite user config automatically without backup,
        // but it's what was requested.
        let home = dirs::home_dir().ok_or_else(|| {
            DevpodError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No home dir",
            ))
        })?;
        let default_kubeconfig = home.join(".kube").join("config");

        // Ensure .kube dir exists
        std::fs::create_dir_all(default_kubeconfig.parent().unwrap())?;

        // Read and parse patched kubeconfig
        let incoming: crate::util::kubeconfig::Kubeconfig = serde_yaml::from_str(&patched_content)
            .map_err(|e| {
                DevpodError::KubeconfigMerge(format!("Failed to parse patched kubeconfig: {}", e))
            })?;

        let mut base_config = if default_kubeconfig.exists() {
            let content = std::fs::read_to_string(&default_kubeconfig)?;
            serde_yaml::from_str(&content).unwrap_or_default()
        } else {
            crate::util::kubeconfig::Kubeconfig::default()
        };

        crate::util::kubeconfig::merge_kubeconfig(&mut base_config, incoming);

        let merged_yaml = serde_yaml::to_string(&base_config)?;
        std::fs::write(&default_kubeconfig, merged_yaml)?;

        info!("Kubeconfig merged");
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
            match self
                .executor
                .execute(host, user, "sudo k3s kubectl get nodes")
                .await
            {
                Ok(_) => {
                    info!("API server ready on {}", host);
                    return Ok(());
                }
                Err(_) => {
                    if i >= max_retries - 1 {
                        return Err(DevpodError::Execution {
                            host: host.to_string(),
                            msg: "Timed out waiting for API server".into(),
                        });
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
        let cluster = config
            .get_cluster(&self.env_name)
            .ok_or_else(|| DevpodError::ClusterNotFound(self.env_name.clone()))?;

        if cluster.provider != "k3s" {
            return Err(DevpodError::Config("Remote provider must be 'k3s'".into()));
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
            return Err(DevpodError::Config(
                "No server node defined for remote cluster".into(),
            ));
        }

        let first_server_ip = servers[0].address.clone();

        // 2. Determine Cluster Token
        // Check if token already exists on the first server (idempotency)
        let token = match self
            .executor
            .execute(
                &first_server_ip,
                user,
                "sudo cat /var/lib/rancher/k3s/server/node-token",
            )
            .await
        {
            Ok(t) => {
                info!("Reusing existing cluster token from {}", first_server_ip);
                t.trim().to_string()
            }
            Err(_) => {
                let t = Self::generate_token();
                info!("Generated new cluster token");
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
            info!("Provisioning HA Cluster with External Datastore...");

            for (i, server) in servers.iter().enumerate() {
                info!("Provisioning Server Node {}: {}", i, server.address);

                let cmd = format!(
                    "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --datastore-endpoint=\"{}\" --tls-san {}",
                    token, runtime_flag(&server.runtime), ds_endpoint, server.address
                );

                match self.executor.execute(&server.address, user, &cmd).await {
                    Ok(_) => info!("K3s server installed on {}", server.address),
                    Err(e) => {
                        tracing::error!(
                            "K3s installation failed on {}. Fetching logs...",
                            server.address
                        );
                        let logs = self
                            .executor
                            .execute(
                                &server.address,
                                user,
                                "sudo journalctl -xeu k3s.service --no-pager -n 50",
                            )
                            .await
                            .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        tracing::error!("K3s Service Logs:\n{}", logs);
                        return Err(e);
                    }
                }
            }
        } else if servers.len() > 1 {
            info!("Provisioning HA Cluster with Embedded Etcd...");

            // Server 0: --cluster-init
            let primary = servers[0];
            info!("Initializing Cluster on Primary: {}", primary.address);

            let cmd_init = format!(
                "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --cluster-init --tls-san {}",
                token, runtime_flag(&primary.runtime), primary.address
            );

            match self
                .executor
                .execute(&primary.address, user, &cmd_init)
                .await
            {
                Ok(_) => info!("Primary initialized"),
                Err(e) => {
                    tracing::error!(
                        "K3s initialization failed on {}. Fetching logs...",
                        primary.address
                    );
                    let logs = self
                        .executor
                        .execute(
                            &primary.address,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    tracing::error!("K3s Service Logs:\n{}", logs);
                    return Err(e);
                }
            }

            // Wait for API server to be ready before joining other servers
            info!("Waiting for API server on {}...", primary.address);
            self.wait_for_api_server(&primary.address, user).await?;

            // Other Servers join
            for server in servers.iter().skip(1) {
                info!("Joining Server: {}", server.address);
                let cmd_join = format!(
                    "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --server https://{}:6443 --tls-san {}",
                    token, runtime_flag(&server.runtime), first_server_ip, server.address
                );

                match self
                    .executor
                    .execute(&server.address, user, &cmd_join)
                    .await
                {
                    Ok(_) => info!("Server join complete"),
                    Err(e) => {
                        tracing::error!("K3s join failed on {}. Fetching logs...", server.address);
                        let logs = self
                            .executor
                            .execute(
                                &server.address,
                                user,
                                "sudo journalctl -xeu k3s.service --no-pager -n 50",
                            )
                            .await
                            .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        tracing::error!("K3s Service Logs:\n{}", logs);
                        return Err(e);
                    }
                }
            }
        } else {
            // Single Server
            let primary = servers[0];
            info!("Provisioning Single-Node Server: {}", primary.address);

            let install_cmd = format!(
                "curl -sfL https://get.k3s.io | K3S_TOKEN={} sh -s - server {} --tls-san {}",
                token,
                runtime_flag(&primary.runtime),
                primary.address
            );

            match self
                .executor
                .execute(&primary.address, user, &install_cmd)
                .await
            {
                Ok(_) => info!("K3s server installed on {}", primary.address),
                Err(e) => {
                    tracing::error!(
                        "K3s installation failed on {}. Fetching logs...",
                        primary.address
                    );
                    let logs = self
                        .executor
                        .execute(
                            &primary.address,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    tracing::error!("K3s Service Logs:\n{}", logs);
                    return Err(e);
                }
            }
        }

        // 4. Join Agents (Common for all modes)
        if !agents.is_empty() {
            // Wait for API server to be ready before joining agents
            info!("Waiting for API server on {}...", first_server_ip);
            self.wait_for_api_server(&first_server_ip, user).await?;

            info!("Joining Agents...");
            for agent in agents {
                info!("Joining Agent: {}", agent.address);
                let join_cmd = format!(
                    "curl -sfL https://get.k3s.io | K3S_URL=https://{}:6443 K3S_TOKEN={} sh -s - agent {}",
                    first_server_ip, token, runtime_flag(&agent.runtime)
                );

                self.executor
                    .execute(&agent.address, user, &join_cmd)
                    .await?;
                info!("Agent joined");
            }
        }

        // 5. Fetch Kubeconfig
        self.fetch_and_merge_kubeconfig(&first_server_ip, user)
            .await?;

        Ok(())
    }

    async fn down(&self, config: &DevpodConfig) -> Result<()> {
        let cluster = config
            .get_cluster(&self.env_name)
            .ok_or_else(|| DevpodError::ClusterNotFound(self.env_name.clone()))?;

        if cluster.provider != "k3s" {
            return Err(DevpodError::Config("Remote provider must be 'k3s'".into()));
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
            info!("Uninstalling K3s Agent on {}...", agent.address);
            // Ignore errors if already uninstalled or unreachable?
            // "k3s-agent-uninstall.sh" usually available in path
            let _ = self
                .executor
                .execute(
                    &agent.address,
                    user,
                    "/usr/local/bin/k3s-agent-uninstall.sh",
                )
                .await;

            // Clean up data
            let _ = self
                .executor
                .execute(
                    &agent.address,
                    user,
                    "sudo rm -rf /var/lib/rancher/k3s /etc/rancher/k3s",
                )
                .await;
            info!("Agent uninstalled & data purged");
        }

        // 3. Uninstall Servers
        for server in servers {
            info!("Uninstalling K3s Server on {}...", server.address);
            let _ = self
                .executor
                .execute(&server.address, user, "/usr/local/bin/k3s-uninstall.sh")
                .await;

            // Clean up data
            let _ = self
                .executor
                .execute(
                    &server.address,
                    user,
                    "sudo rm -rf /var/lib/rancher/k3s /etc/rancher/k3s",
                )
                .await;
            info!("Server uninstalled & data purged");
        }

        // 4. Cleanup Local Kubeconfig
        let context_name = format!("devpod-{}", self.env_name);
        info!("Cleaning up local context '{}'...", context_name);

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

        info!("Local cleanup complete");

        Ok(())
    }

    async fn sync_images(&self, _images: Vec<PathBuf>) -> Result<()> {
        // Placeholder for image sync logic (scp + ctr import)
        Ok(())
    }

    async fn apply_manifests(&self, yaml_path: PathBuf) -> Result<()> {
        info!("Applying manifests to remote...");
        // Use the context we just created/merged
        let context_name = format!("devpod-{}", self.env_name);

        let status = Command::new("kubectl")
            .arg("--context")
            .arg(&context_name)
            .args(["apply", "-f", yaml_path.to_str().unwrap(), "--recursive"])
            .status()
            .await
            .map_err(|e| DevpodError::Command(format!("Failed to apply manifests: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to apply manifests".into()));
        }
        Ok(())
    }

    async fn sync_secrets(&self, config: &DevpodConfig) -> Result<()> {
        if !config.secrets.enabled {
            return Ok(());
        }

        let secret_set = config.secrets.set.as_deref().unwrap_or("default");
        let context_name = format!("devpod-{}", self.env_name);

        info!(
            "Syncing secrets '{}' to remote context '{}'...",
            secret_set, context_name
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
            .map_err(|e| DevpodError::Command(format!("Failed to run ksecret sync: {}", e)))?;

        if !status.success() {
            return Err(DevpodError::Command("Failed to sync secrets".into()));
        }

        info!("Secrets synced");
        Ok(())
    }
}
