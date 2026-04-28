use crate::config::{ClusterDefinition, DevpodConfig, PublishedPortConfig, RemoteNodeConfig};
use crate::executor::RemoteExecutor;
use crate::orchestrator::ClusterManager;
use crate::util::kubeconfig::Kubeconfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use colored::Colorize;
use serde::Deserialize;
use std::collections::HashSet;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

pub struct RemoteManager {
    env_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndpointKind {
    Tailnet,
    Lan,
    Direct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EndpointSpec {
    kind: EndpointKind,
    context_name: String,
    server_host: String,
}

#[derive(Debug, Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "Self")]
    me: Option<TailscaleSelf>,
}

#[derive(Debug, Deserialize)]
struct TailscaleSelf {
    #[serde(rename = "ID")]
    id: String,
}

impl RemoteManager {
    pub fn new(env_name: &str) -> Self {
        Self {
            env_name: env_name.to_string(),
        }
    }

    fn legacy_context_name(&self) -> String {
        format!("devpod-{}", self.env_name)
    }

    fn context_name(&self, suffix: &str) -> String {
        format!("devpod-{}-{}", self.env_name, suffix)
    }

    fn endpoint_specs(
        &self,
        cluster: &ClusterDefinition,
        primary: &RemoteNodeConfig,
    ) -> Vec<EndpointSpec> {
        let mut endpoints = Vec::new();
        let include_lan = matches!(cluster.access_mode(), "dual" | "lan-only");
        let include_tailnet = cluster.tailscale_enabled() && cluster.tailnet_domain().is_some();

        if include_tailnet {
            endpoints.push(EndpointSpec {
                kind: EndpointKind::Tailnet,
                context_name: self.context_name("tailnet"),
                server_host: primary.tailscale_hostname(cluster.tailnet_domain().unwrap()),
            });
        }

        if include_lan {
            endpoints.push(EndpointSpec {
                kind: EndpointKind::Lan,
                context_name: self.context_name("lan"),
                server_host: primary.lan_hostname(cluster.lan_domain()),
            });
        }

        if let Some(address) = primary.bootstrap_address() {
            endpoints.push(EndpointSpec {
                kind: EndpointKind::Direct,
                context_name: self.context_name("direct"),
                server_host: address.to_string(),
            });
        }

        if cluster.prefers_lan() {
            endpoints.sort_by_key(|endpoint| match endpoint.kind {
                EndpointKind::Lan => 0,
                EndpointKind::Tailnet => 1,
                EndpointKind::Direct => 2,
            });
        }

        endpoints
    }

    fn preferred_context_name(
        &self,
        cluster: &ClusterDefinition,
        primary: &RemoteNodeConfig,
    ) -> String {
        self.endpoint_specs(cluster, primary)
            .into_iter()
            .next()
            .map(|endpoint| endpoint.context_name)
            .unwrap_or_else(|| self.context_name("lan"))
    }

    fn endpoint_reachable(host: &str, port: u16) -> bool {
        let timeout = Duration::from_secs(3);
        let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
            Ok(addrs) => addrs.collect(),
            Err(_) => return false,
        };

        addrs
            .into_iter()
            .any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
    }

    fn current_context_name(&self, cluster: &ClusterDefinition, primary: &RemoteNodeConfig) -> String {
        let endpoints = self.endpoint_specs(cluster, primary);

        if let Some(endpoint) = endpoints
            .iter()
            .find(|endpoint| Self::endpoint_reachable(&endpoint.server_host, 6443))
        {
            return endpoint.context_name.clone();
        }

        self.preferred_context_name(cluster, primary)
    }

    fn connection_candidates(
        &self,
        cluster: &ClusterDefinition,
        node: &RemoteNodeConfig,
    ) -> Vec<String> {
        let mut candidates = Vec::new();

        if cluster.tailscale_enabled() {
            if let Some(domain) = cluster.tailnet_domain() {
                candidates.push(node.tailscale_hostname(domain));
            }
        }

        if cluster.access_mode() != "tailscale-only" {
            candidates.push(node.lan_hostname(cluster.lan_domain()));
        }

        if let Some(address) = node.bootstrap_address() {
            candidates.push(address.to_string());
        }

        dedupe_strings(candidates)
    }

    async fn resolve_node_host(
        &self,
        cluster: &ClusterDefinition,
        node: &RemoteNodeConfig,
        user: &str,
    ) -> Result<String> {
        let candidates = self.connection_candidates(cluster, node);
        let candidate_refs: Vec<_> = candidates.iter().map(String::as_str).collect();

        if let Some(host) =
            RemoteExecutor::first_reachable(candidate_refs.iter().copied(), user).await
        {
            return Ok(host);
        }

        candidates
            .into_iter()
            .next()
            .context("No connection targets available for node")
    }

    async fn fetch_kubeconfig(&self, host: &str, user: &str) -> Result<Kubeconfig> {
        println!("{} Fetching kubeconfig...", "->".blue());

        let temp_dir = tempfile::tempdir()?;
        let temp_kubeconfig = temp_dir.path().join("k3s.yaml");
        let temp_path_str = temp_kubeconfig.to_str().unwrap();
        let remote_temp = "/tmp/k3s.yaml";

        RemoteExecutor::execute(
            host,
            user,
            &format!(
                "sudo cp /etc/rancher/k3s/k3s.yaml {} && sudo chmod 644 {}",
                remote_temp, remote_temp
            ),
        )
        .await?;

        if let Err(error) = RemoteExecutor::scp_from(host, user, remote_temp, temp_path_str).await {
            let _ = RemoteExecutor::execute(host, user, &format!("sudo rm {}", remote_temp)).await;
            return Err(error);
        }

        let _ = RemoteExecutor::execute(host, user, &format!("sudo rm {}", remote_temp)).await;

        let content = std::fs::read_to_string(&temp_kubeconfig)?;
        serde_yaml::from_str(&content).context("Failed to parse remote kubeconfig")
    }

    fn rewrite_kubeconfig_for_endpoint(
        source: &Kubeconfig,
        context_name: &str,
        server_host: &str,
    ) -> Kubeconfig {
        let mut updated = source.clone();
        let server_url = format!("https://{}:6443", server_host);

        for cluster in &mut updated.clusters {
            cluster.name = context_name.to_string();
            cluster.cluster.server = server_url.clone();
        }

        for user in &mut updated.users {
            user.name = context_name.to_string();
        }

        for context in &mut updated.contexts {
            context.name = context_name.to_string();
            context.context.cluster = context_name.to_string();
            context.context.user = context_name.to_string();
        }

        updated.current_context = context_name.to_string();
        updated
    }

    fn merge_kubeconfigs(&self, configs: Vec<Kubeconfig>, current_context: &str) -> Result<()> {
        let home = dirs::home_dir().context("No home dir")?;
        let default_kubeconfig = home.join(".kube").join("config");
        std::fs::create_dir_all(default_kubeconfig.parent().unwrap())?;

        let mut base_config = if default_kubeconfig.exists() {
            let content = std::fs::read_to_string(&default_kubeconfig)?;
            serde_yaml::from_str(&content).unwrap_or_default()
        } else {
            Kubeconfig::default()
        };

        for incoming in configs {
            crate::util::kubeconfig::merge_kubeconfig(&mut base_config, incoming);
        }

        base_config.current_context = current_context.to_string();
        let merged_yaml = serde_yaml::to_string(&base_config)?;
        std::fs::write(&default_kubeconfig, merged_yaml)?;
        Ok(())
    }

    async fn fetch_and_merge_kubeconfig(
        &self,
        cluster: &ClusterDefinition,
        primary: &RemoteNodeConfig,
        host: &str,
        user: &str,
    ) -> Result<()> {
        let source = self.fetch_kubeconfig(host, user).await?;
        let endpoints = self.endpoint_specs(cluster, primary);

        if endpoints.is_empty() {
            anyhow::bail!(
                "No portable kubeconfig endpoints are available. Configure LAN or Tailscale access."
            );
        }

        let current_context = self.current_context_name(cluster, primary);
        let configs = endpoints
            .into_iter()
            .map(|endpoint| {
                Self::rewrite_kubeconfig_for_endpoint(
                    &source,
                    &endpoint.context_name,
                    &endpoint.server_host,
                )
            })
            .collect();

        println!(
            "{} Merging kubeconfig contexts for '{}'",
            "->".blue(),
            self.env_name
        );
        self.merge_kubeconfigs(configs, &current_context)?;
        println!("{} Kubeconfig merged", "OK".green());
        Ok(())
    }

    fn generate_token() -> String {
        use std::process::Command as StdCommand;

        if let Ok(output) = StdCommand::new("openssl")
            .args(["rand", "-hex", "32"])
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }

        if let Ok(output) = StdCommand::new("sh")
            .arg("-c")
            .arg("head -c 32 /dev/urandom | xxd -p -c 64")
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
        }

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
        for attempt in 0..max_retries {
            match RemoteExecutor::execute(host, user, "sudo k3s kubectl get nodes").await {
                Ok(_) => {
                    println!("   {} API server ready on {}", "OK".green(), host);
                    return Ok(());
                }
                Err(_) if attempt >= max_retries - 1 => {
                    anyhow::bail!("Timed out waiting for API server on {}", host);
                }
                Err(_) => tokio::time::sleep(tokio::time::Duration::from_secs(3)).await,
            }
        }

        Ok(())
    }

    fn runtime_flag(runtime: &str) -> &str {
        if runtime == "docker" {
            "--docker"
        } else {
            ""
        }
    }

    fn tls_sans(cluster: &ClusterDefinition, primary: &RemoteNodeConfig) -> Vec<String> {
        let mut sans = Vec::new();

        if let Some(address) = primary.bootstrap_address() {
            sans.push(address.to_string());
        }

        sans.push(primary.lan_hostname(cluster.lan_domain()));

        if cluster.tailscale_enabled() {
            if let Some(domain) = cluster.tailnet_domain() {
                sans.push(primary.tailscale_hostname(domain));
            }
        }

        dedupe_strings(sans)
    }

    fn build_k3s_server_install_cmd(
        cluster: &ClusterDefinition,
        node: &RemoteNodeConfig,
        token: &str,
        datastore: Option<&str>,
        cluster_init: bool,
        join_server: Option<&str>,
        primary: &RemoteNodeConfig,
    ) -> String {
        let runtime = Self::runtime_flag(&node.runtime);
        let mut parts = vec!["curl -sfL https://get.k3s.io |".to_string()];

        parts.push(format!("K3S_TOKEN={} sh -s - server", token));
        if !runtime.is_empty() {
            parts.push(runtime.to_string());
        }
        if let Some(endpoint) = datastore {
            parts.push(format!("--datastore-endpoint=\"{}\"", endpoint));
        }
        if cluster_init {
            parts.push("--cluster-init".to_string());
        }
        if let Some(server) = join_server {
            parts.push(format!("--server https://{}:6443", server));
        }

        for san in Self::tls_sans(cluster, primary) {
            parts.push(format!("--tls-san {}", san));
        }

        parts.join(" ")
    }

    fn build_k3s_agent_install_cmd(
        agent: &RemoteNodeConfig,
        primary_host: &str,
        token: &str,
    ) -> String {
        let runtime = Self::runtime_flag(&agent.runtime);
        let mut parts = vec![
            "curl -sfL https://get.k3s.io |".to_string(),
            format!("K3S_URL=https://{}:6443", primary_host),
            format!("K3S_TOKEN={} sh -s - agent", token),
        ];

        if !runtime.is_empty() {
            parts.push(runtime.to_string());
        }

        parts.join(" ")
    }

    fn build_tailscale_up_cmd(cluster: &ClusterDefinition, auth_key: &str) -> String {
        let mut parts = vec![
            "sudo tailscale up".to_string(),
            format!("--auth-key={}", auth_key),
        ];

        if cluster.tailscale.ssh {
            parts.push("--ssh".to_string());
        }

        if !cluster.tailscale.tags.is_empty() {
            parts.push(format!(
                "--advertise-tags={}",
                cluster.tailscale.tags.join(",")
            ));
        }

        parts.push("--accept-risk=lose-ssh".to_string());
        parts.join(" ")
    }

    fn tailscale_connected_check_cmd() -> &'static str {
        "if sudo tailscale ip -4 2>/dev/null | grep -q . || sudo tailscale ip -6 2>/dev/null | grep -q .; then echo connected; else echo logged-out; fi"
    }

    async fn tailscale_debug_info(&self, host: &str, user: &str) -> String {
        RemoteExecutor::execute(
            host,
            user,
            "sh -lc 'echo \"== tailscale status ==\"; sudo tailscale status --json 2>&1 || true; echo; echo \"== tailscaled log ==\"; sudo journalctl -u tailscaled --no-pager -n 20 2>&1 || true'",
        )
        .await
        .unwrap_or_else(|error| format!("Failed to fetch Tailscale diagnostics: {}", error))
    }

    async fn fetch_tailscale_device_id(&self, host: &str, user: &str) -> Option<String> {
        let status = RemoteExecutor::execute(host, user, "sudo tailscale status --json")
            .await
            .ok()?;
        let parsed: TailscaleStatus = serde_json::from_str(&status).ok()?;
        parsed.me.map(|me| me.id)
    }

    async fn delete_tailscale_device(
        &self,
        cluster: &ClusterDefinition,
        node_name: &str,
        device_id: &str,
    ) -> Result<()> {
        let api_key = std::env::var(&cluster.tailscale.api_key_env).with_context(|| {
            format!(
                "Tailscale API deletion requested but env var '{}' is not set",
                cluster.tailscale.api_key_env
            )
        })?;

        let response = reqwest::Client::new()
            .delete(format!("https://api.tailscale.com/api/v2/device/{}", device_id))
            .basic_auth(api_key, Some(""))
            .send()
            .await
            .with_context(|| {
                format!("Failed to call Tailscale API while deleting '{}'", node_name)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            anyhow::bail!(
                "Tailscale API device deletion failed for '{}': {} {}",
                node_name,
                status,
                body
            );
        }

        Ok(())
    }

    fn tailscale_purge_cmd() -> &'static str {
        "sudo sh -lc 'tailscale serve reset >/dev/null 2>&1 || true; tailscale down >/dev/null 2>&1 || true; tailscale logout >/dev/null 2>&1 || true; systemctl stop tailscaled >/dev/null 2>&1 || true; systemctl disable tailscaled >/dev/null 2>&1 || true; apt-get purge -y tailscale >/dev/null 2>&1 || true; apt-get autoremove -y >/dev/null 2>&1 || true; rm -rf /var/lib/tailscale /etc/default/tailscaled /etc/systemd/system/tailscaled.service.d /usr/share/keyrings/tailscale-archive-keyring.gpg /etc/apt/sources.list.d/tailscale.list'"
    }

    fn build_tailscale_serve_cmd(port: &PublishedPortConfig) -> String {
        let protocol = port.protocol.to_ascii_lowercase();
        match protocol.as_str() {
            "http" => format!(
                "sudo tailscale serve --yes --bg --http={} localhost:{}",
                port.port, port.port
            ),
            "https" => format!(
                "sudo tailscale serve --yes --bg --https={} localhost:{}",
                port.port, port.port
            ),
            _ => format!(
                "sudo tailscale serve --yes --bg --tcp={} tcp://localhost:{}",
                port.port, port.port
            ),
        }
    }

    async fn ensure_node_base_access(
        &self,
        cluster: &ClusterDefinition,
        node: &RemoteNodeConfig,
        user: &str,
    ) -> Result<()> {
        let bootstrap = node
            .bootstrap_address()
            .context("Remote node is missing bootstrap_address/address")?;
        let hostname = node.stable_name();

        RemoteExecutor::execute(
            bootstrap,
            user,
            &format!(
                "sudo apt-get update && sudo apt-get install -y curl avahi-daemon && sudo hostnamectl set-hostname {} && sudo sh -c 'if grep -q \"^127.0.1.1[[:space:]]\" /etc/hosts; then sed -i \"s/^127.0.1.1[[:space:]].*/127.0.1.1 {}/\" /etc/hosts; else printf \"127.0.1.1 {}\\n\" >> /etc/hosts; fi' && sudo systemctl enable --now avahi-daemon",
                hostname,
                hostname,
                hostname
            ),
        )
        .await?;

        if !cluster.tailscale_enabled() {
            return Ok(());
        }

        println!(
            "{} Ensuring Tailscale is installed on {}...",
            "->".blue(),
            hostname
        );
        RemoteExecutor::execute(
            bootstrap,
            user,
            "curl -fsSL https://tailscale.com/install.sh | sh",
        )
        .await?;

        println!(
            "{} Checking Tailscale session on {}...",
            "->".blue(),
            hostname
        );
        let tailscale_status = RemoteExecutor::execute(
            bootstrap,
            user,
            Self::tailscale_connected_check_cmd(),
        )
        .await
        .unwrap_or_default();

        if tailscale_status.trim() == "connected" {
            println!(
                "{} Tailscale already connected on {}",
                "OK".green(),
                hostname
            );
            return Ok(());
        }

        let auth_key = std::env::var(&cluster.tailscale.auth_key_env).with_context(|| {
            format!(
                "Tailscale is enabled but env var '{}' is not set",
                cluster.tailscale.auth_key_env
            )
        })?;

        println!("{} Bringing Tailscale up on {}...", "->".blue(), hostname);
        let cmd = Self::build_tailscale_up_cmd(cluster, &auth_key);
        if let Err(error) = RemoteExecutor::execute(bootstrap, user, &cmd).await {
            let diagnostics = self.tailscale_debug_info(bootstrap, user).await;
            anyhow::bail!(
                "Failed to bring Tailscale up on '{}': {}\n{}",
                hostname,
                error,
                diagnostics
            );
        }

        let tailscale_status = RemoteExecutor::execute(
            bootstrap,
            user,
            Self::tailscale_connected_check_cmd(),
        )
        .await
        .unwrap_or_default();

        if tailscale_status.trim() != "connected" {
            let diagnostics = self.tailscale_debug_info(bootstrap, user).await;
            anyhow::bail!(
                "Tailscale did not reach a running state on '{}'. Check the auth key in '{}' and confirm it is allowed to join the tailnet with the requested tags.\n{}",
                hostname,
                cluster.tailscale.auth_key_env,
                diagnostics
            );
        }

        println!("{} Tailscale connected on {}", "OK".green(), hostname);
        Ok(())
    }

    async fn ensure_k3s_prereqs(&self, node: &RemoteNodeConfig, user: &str) -> Result<()> {
        let bootstrap = node
            .bootstrap_address()
            .context("Remote node is missing bootstrap_address/address")?;

        let check = RemoteExecutor::execute(
            bootstrap,
            user,
            "if [ -f /sys/fs/cgroup/cgroup.controllers ] && grep -qw memory /sys/fs/cgroup/cgroup.controllers; then echo ok; else echo missing; fi",
        )
        .await?;

        if check.trim() == "ok" {
            return Ok(());
        }

        anyhow::bail!(
            "Node '{}' is missing the memory cgroup controller required by k3s. Run `devpod setup --env {}` and let the node reboot, or enable `cgroup_memory=1 cgroup_enable=memory` in the boot cmdline and reboot before retrying `devpod up`.",
            node.stable_name(),
            self.env_name
        );
    }

    async fn configure_published_ports(
        &self,
        cluster: &ClusterDefinition,
        user: &str,
    ) -> Result<()> {
        if !cluster.tailscale_enabled() || cluster.access.published_ports.is_empty() {
            return Ok(());
        }

        for port in &cluster.access.published_ports {
            let valid_nodes = cluster
                .nodes
                .iter()
                .map(Self::node_ref_summary)
                .collect::<Vec<_>>()
                .join(", ");
            let node = cluster
                .nodes
                .iter()
                .find(|node| node.matches_node_ref(&port.node))
                .with_context(|| {
                    format!(
                        "Published port node '{}' not found. Valid node refs: {}",
                        port.node, valid_nodes
                    )
                })?;

            let target = self.resolve_node_host(cluster, node, user).await?;
            let cmd = Self::build_tailscale_serve_cmd(port);
            RemoteExecutor::execute(&target, user, &cmd).await?;

            println!(
                "{} Published {} on {} via Tailscale Serve",
                "OK".green(),
                port.name.as_deref().unwrap_or("service"),
                node.stable_name()
            );
        }

        Ok(())
    }

    fn node_ref_summary(node: &RemoteNodeConfig) -> String {
        let mut refs = vec![node.stable_name()];

        if let Some(name) = node.name.as_deref() {
            if !name.trim().is_empty() {
                refs.push(name.to_string());
            }
        }

        if let Some(address) = node.bootstrap_address() {
            refs.push(address.to_string());
        }

        refs.sort();
        refs.dedup();
        refs.join("/")
    }

    async fn uninstall_node(
        &self,
        cluster: &ClusterDefinition,
        node: &RemoteNodeConfig,
        user: &str,
        uninstall_cmd: &str,
    ) {
        let node_name = node.stable_name();
        let target = self
            .resolve_node_host(cluster, node, user)
            .await
            .or_else(|_| {
                node.bootstrap_address()
                    .map(str::to_string)
                    .context("No reachable node target")
            });

        if let Ok(target) = target {
            let tailscale_device_id = if cluster.tailscale_enabled() {
                self.fetch_tailscale_device_id(&target, user).await
            } else {
                None
            };

            let _ = RemoteExecutor::execute(&target, user, uninstall_cmd).await;
            let _ = RemoteExecutor::execute(
                &target,
                user,
                "sudo rm -rf /var/lib/rancher/k3s /etc/rancher/k3s",
            )
            .await;

            if cluster.tailscale_enabled() {
                let _ = RemoteExecutor::execute(&target, user, Self::tailscale_purge_cmd()).await;

                if let Some(device_id) = tailscale_device_id {
                    match self
                        .delete_tailscale_device(cluster, &node_name, &device_id)
                        .await
                    {
                        Ok(_) => println!(
                            "   {} Tailscale device '{}' deleted from tailnet",
                            "OK".green(),
                            node_name
                        ),
                        Err(error) => println!(
                            "{} Tailscale device '{}' was purged locally but not deleted from the tailnet: {}",
                            "!".yellow(),
                            node_name,
                            error
                        ),
                    }
                } else {
                    println!(
                        "{} Tailscale device ID for '{}' could not be determined; local Tailscale state was purged but admin-console deletion was skipped",
                        "!".yellow(),
                        node_name
                    );
                }
            }
        }
    }
}

#[async_trait(?Send)]
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
        let servers: Vec<RemoteNodeConfig> = cluster
            .nodes
            .iter()
            .filter(|node| node.role == "server")
            .cloned()
            .collect();
        let agents: Vec<RemoteNodeConfig> = cluster
            .nodes
            .iter()
            .filter(|node| node.role == "agent")
            .cloned()
            .collect();

        if servers.is_empty() {
            anyhow::bail!("No server node defined for remote cluster");
        }

        let primary = servers[0].clone();
        let primary_bootstrap = primary
            .bootstrap_address()
            .context("Primary server is missing bootstrap_address/address")?
            .to_string();

        println!("{} Preparing remote node access...", "->".blue().bold());
        for node in cluster.nodes.clone() {
            println!("{} Preparing {}", "->".blue(), node.stable_name());
            self.ensure_node_base_access(cluster, &node, user).await?;
            self.ensure_k3s_prereqs(&node, user).await?;
        }

        let token = match RemoteExecutor::execute(
            &primary_bootstrap,
            user,
            "sudo cat /var/lib/rancher/k3s/server/node-token",
        )
        .await
        {
            Ok(token) => {
                println!(
                    "{} Reusing existing cluster token from {}",
                    "OK".green(),
                    primary.stable_name()
                );
                token.trim().to_string()
            }
            Err(_) => {
                let token = Self::generate_token();
                println!("{} Generated new cluster token", "OK".green());
                token
            }
        };

        if let Some(datastore) = cluster.datastore_endpoint.as_deref() {
            println!(
                "{} Provisioning HA cluster with external datastore...",
                "->".blue().bold()
            );

            for server in servers.clone() {
                let bootstrap = server
                    .bootstrap_address()
                    .context("Server node is missing bootstrap_address/address")?;
                let cmd = Self::build_k3s_server_install_cmd(
                    cluster,
                    &server,
                    &token,
                    Some(datastore),
                    false,
                    None,
                    &primary,
                );

                match RemoteExecutor::execute(bootstrap, user, &cmd).await {
                    Ok(_) => println!(
                        "   {} K3s server installed on {}",
                        "OK".green(),
                        server.stable_name()
                    ),
                    Err(error) => {
                        let logs = RemoteExecutor::execute(
                            bootstrap,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                        return Err(error);
                    }
                }
            }
        } else if servers.len() > 1 {
            println!(
                "{} Provisioning HA cluster with embedded etcd...",
                "->".blue().bold()
            );

            let primary_init_cmd = Self::build_k3s_server_install_cmd(
                cluster, &primary, &token, None, true, None, &primary,
            );

            match RemoteExecutor::execute(&primary_bootstrap, user, &primary_init_cmd).await {
                Ok(_) => println!("   {} Primary initialized", "OK".green()),
                Err(error) => {
                    let logs = RemoteExecutor::execute(
                        &primary_bootstrap,
                        user,
                        "sudo journalctl -xeu k3s.service --no-pager -n 50",
                    )
                    .await
                    .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                    return Err(error);
                }
            }

            println!(
                "{} Waiting for API server on {}...",
                "->".blue(),
                primary.stable_name()
            );
            self.wait_for_api_server(&primary_bootstrap, user).await?;

            for server in servers.iter().skip(1).cloned() {
                let bootstrap = server
                    .bootstrap_address()
                    .context("Server node is missing bootstrap_address/address")?;
                let cmd = Self::build_k3s_server_install_cmd(
                    cluster,
                    &server,
                    &token,
                    None,
                    false,
                    Some(&primary_bootstrap),
                    &primary,
                );

                match RemoteExecutor::execute(bootstrap, user, &cmd).await {
                    Ok(_) => println!("   {} Server join complete", "OK".green()),
                    Err(error) => {
                        let logs = RemoteExecutor::execute(
                            bootstrap,
                            user,
                            "sudo journalctl -xeu k3s.service --no-pager -n 50",
                        )
                        .await
                        .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                        println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                        return Err(error);
                    }
                }
            }
        } else {
            println!(
                "{} Provisioning single-node server {}...",
                "->".blue().bold(),
                primary.stable_name()
            );

            let cmd = Self::build_k3s_server_install_cmd(
                cluster, &primary, &token, None, false, None, &primary,
            );

            match RemoteExecutor::execute(&primary_bootstrap, user, &cmd).await {
                Ok(_) => println!(
                    "   {} K3s server installed on {}",
                    "OK".green(),
                    primary.stable_name()
                ),
                Err(error) => {
                    let logs = RemoteExecutor::execute(
                        &primary_bootstrap,
                        user,
                        "sudo journalctl -xeu k3s.service --no-pager -n 50",
                    )
                    .await
                    .unwrap_or_else(|_| "Failed to fetch logs".to_string());
                    println!("{} K3s Service Logs:\n{}", "->".blue(), logs);
                    return Err(error);
                }
            }
        }

        if !agents.is_empty() {
            println!(
                "{} Waiting for API server on {}...",
                "->".blue(),
                primary.stable_name()
            );
            self.wait_for_api_server(&primary_bootstrap, user).await?;

            println!("{} Joining agents...", "->".blue());
            for agent in &agents {
                let bootstrap = agent
                    .bootstrap_address()
                    .context("Agent node is missing bootstrap_address/address")?;
                let cmd = Self::build_k3s_agent_install_cmd(&agent, &primary_bootstrap, &token);
                RemoteExecutor::execute(bootstrap, user, &cmd).await?;
                println!("   {} Agent joined", "OK".green());
            }
        }

        self.configure_published_ports(cluster, user).await?;

        let kubeconfig_target = self.resolve_node_host(cluster, &primary, user).await?;
        self.fetch_and_merge_kubeconfig(cluster, &primary, &kubeconfig_target, user)
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
        let servers: Vec<RemoteNodeConfig> = cluster
            .nodes
            .iter()
            .filter(|node| node.role == "server")
            .cloned()
            .collect();
        let agents: Vec<RemoteNodeConfig> = cluster
            .nodes
            .iter()
            .filter(|node| node.role == "agent")
            .cloned()
            .collect();

        for agent in agents {
            println!(
                "{} Uninstalling K3s agent on {}...",
                "->".blue(),
                agent.stable_name()
            );
            self.uninstall_node(
                cluster,
                &agent,
                user,
                "/usr/local/bin/k3s-agent-uninstall.sh",
            )
            .await;
            println!("   {} Agent uninstalled & data purged", "OK".green());
        }

        for server in servers {
            println!(
                "{} Uninstalling K3s server on {}...",
                "->".blue(),
                server.stable_name()
            );
            self.uninstall_node(cluster, &server, user, "/usr/local/bin/k3s-uninstall.sh")
                .await;
            println!("   {} Server uninstalled & data purged", "OK".green());
        }

        let mut context_names = vec![
            self.legacy_context_name(),
            self.context_name("lan"),
            self.context_name("tailnet"),
            self.context_name("direct"),
        ];
        context_names.sort();
        context_names.dedup();

        for context_name in context_names {
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
        }

        println!("{} Local cleanup complete", "OK".green());
        Ok(())
    }

    async fn sync_images(&self, _images: Vec<PathBuf>) -> Result<()> {
        Ok(())
    }

    async fn apply_manifests(&self, config: &DevpodConfig, yaml_path: PathBuf) -> Result<()> {
        println!("{} Applying manifests to remote...", "->".blue());
        let cluster = config.get_cluster(&self.env_name).context(format!(
            "Cluster definition for '{}' not found in config",
            self.env_name
        ))?;
        let primary = cluster
            .nodes
            .iter()
            .find(|node| node.role == "server")
            .context("No server node defined for remote cluster")?;
        let context_name = self.current_context_name(cluster, primary);

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

        let cluster = config.get_cluster(&self.env_name).context(format!(
            "Cluster definition for '{}' not found in config",
            self.env_name
        ))?;
        let primary = cluster
            .nodes
            .iter()
            .find(|node| node.role == "server")
            .context("No server node defined for remote cluster")?;
        let context_name = self.current_context_name(cluster, primary);
        let secret_set = config.secrets.set.as_deref().unwrap_or("default");

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

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }

    deduped
}

#[cfg(test)]
mod tests {
    use super::{EndpointKind, RemoteManager};
    use crate::config::{
        ClusterAccessConfig, ClusterDefinition, PublishedPortConfig, RemoteNodeConfig,
        TailscaleConfig,
    };
    use crate::util::kubeconfig::Kubeconfig;
    use std::collections::HashMap;

    fn cluster() -> ClusterDefinition {
        ClusterDefinition {
            provider: "k3s".to_string(),
            connection: Some("ssh".to_string()),
            user: Some("root".to_string()),
            nodes: vec![RemoteNodeConfig {
                role: "server".to_string(),
                name: Some("control-1".to_string()),
                bootstrap_address: Some("192.168.50.10".to_string()),
                address: None,
                runtime: "containerd".to_string(),
                labels: HashMap::new(),
            }],
            datastore_endpoint: None,
            access: ClusterAccessConfig {
                mode: "dual".to_string(),
                primary: "tailscale".to_string(),
                lan_domain: "local".to_string(),
                published_ports: Vec::new(),
            },
            tailscale: TailscaleConfig {
                enabled: true,
                tailnet_domain: Some("example.ts.net".to_string()),
                auth_key_env: "TAILSCALE_AUTH_KEY".to_string(),
                api_key_env: "TAILSCALE_API_KEY".to_string(),
                tags: vec!["tag:k3s".to_string()],
                ssh: true,
            },
        }
    }

    #[test]
    fn endpoint_specs_prefer_tailnet_for_dual_mode() {
        let manager = RemoteManager::new("edge");
        let cluster = cluster();
        let endpoints = manager.endpoint_specs(&cluster, &cluster.nodes[0]);

        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints[0].kind, EndpointKind::Tailnet);
        assert_eq!(endpoints[0].context_name, "devpod-edge-tailnet");
        assert_eq!(endpoints[0].server_host, "control-1.example.ts.net");
        assert_eq!(endpoints[1].kind, EndpointKind::Lan);
        assert_eq!(endpoints[1].context_name, "devpod-edge-lan");
        assert_eq!(endpoints[2].kind, EndpointKind::Direct);
        assert_eq!(endpoints[2].context_name, "devpod-edge-direct");
        assert_eq!(endpoints[2].server_host, "192.168.50.10");
    }

    #[test]
    fn connection_candidates_follow_tailnet_lan_bootstrap_order() {
        let manager = RemoteManager::new("edge");
        let cluster = cluster();
        let candidates = manager.connection_candidates(&cluster, &cluster.nodes[0]);

        assert_eq!(
            candidates,
            vec![
                "control-1.example.ts.net".to_string(),
                "control-1.local".to_string(),
                "192.168.50.10".to_string()
            ]
        );
    }

    #[test]
    fn kubeconfig_rewrite_emits_named_context_and_endpoint() {
        let source: Kubeconfig = serde_yaml::from_str(
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: default
    cluster:
      certificate-authority-data: test
      server: https://127.0.0.1:6443
contexts:
  - name: default
    context:
      cluster: default
      user: default
current-context: default
users:
  - name: default
    user:
      client-certificate-data: cert
      client-key-data: key
"#,
        )
        .unwrap();

        let rewritten = RemoteManager::rewrite_kubeconfig_for_endpoint(
            &source,
            "devpod-edge-tailnet",
            "control-1.example.ts.net",
        );

        assert_eq!(rewritten.current_context, "devpod-edge-tailnet");
        assert_eq!(rewritten.clusters[0].name, "devpod-edge-tailnet");
        assert_eq!(
            rewritten.clusters[0].cluster.server,
            "https://control-1.example.ts.net:6443"
        );
        assert_eq!(rewritten.contexts[0].context.cluster, "devpod-edge-tailnet");
        assert_eq!(rewritten.users[0].name, "devpod-edge-tailnet");
    }

    #[test]
    fn k3s_server_install_command_includes_dual_path_tls_sans() {
        let cluster = cluster();
        let command = RemoteManager::build_k3s_server_install_cmd(
            &cluster,
            &cluster.nodes[0],
            "token123",
            None,
            true,
            None,
            &cluster.nodes[0],
        );

        assert!(command.contains("K3S_TOKEN=token123 sh -s - server"));
        assert!(command.contains("--cluster-init"));
        assert!(command.contains("--tls-san 192.168.50.10"));
        assert!(command.contains("--tls-san control-1.local"));
        assert!(command.contains("--tls-san control-1.example.ts.net"));
    }

    #[test]
    fn tailscale_serve_command_matches_protocol() {
        let http = PublishedPortConfig {
            node: "control-1".to_string(),
            port: 80,
            protocol: "HTTP".to_string(),
            name: None,
        };
        let tcp = PublishedPortConfig {
            node: "control-1".to_string(),
            port: 1883,
            protocol: "TCP".to_string(),
            name: None,
        };

        assert_eq!(
            RemoteManager::build_tailscale_serve_cmd(&http),
            "sudo tailscale serve --yes --bg --http=80 localhost:80"
        );
        assert_eq!(
            RemoteManager::build_tailscale_serve_cmd(&tcp),
            "sudo tailscale serve --yes --bg --tcp=1883 tcp://localhost:1883"
        );
    }
}
