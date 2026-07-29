use crate::config::{DevpodConfig, RemoteNodeConfig};
use crate::executor::RemoteExecutor;
use anyhow::{Context, Result};
use colored::Colorize;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitNodeTarget {
    pub label: String,
    pub host: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitNodesPlan {
    pub env_name: String,
    pub user: String,
    pub targets: Vec<InitNodeTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitNodeOutcome {
    label: String,
    host: String,
    success: bool,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscalePeerInfo {
    pub name: String,
    pub online: Option<bool>,
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshFailureKind {
    ConnectionRejectedBeforeAuth,
    PermissionDenied,
    TimeoutOrNoRoute,
    MissingIdentities,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshFailureDiagnosis {
    pub kind: SshFailureKind,
    pub summary: String,
}

#[derive(Debug, Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "Self")]
    self_node: Option<TailscaleNode>,
    #[serde(rename = "Peer", default)]
    peers: HashMap<String, TailscaleNode>,
}

#[derive(Debug, Deserialize)]
struct TailscaleNode {
    #[serde(rename = "HostName", default)]
    host_name: Option<String>,
    #[serde(rename = "DNSName", default)]
    dns_name: Option<String>,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscaleCommand {
    pub program: String,
    pub args: Vec<String>,
}

pub fn ssh_copy_id_args(user: &str, host: &str, identity: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];

    if let Some(identity) = identity {
        args.push("-i".to_string());
        args.push(identity.display().to_string());
    }

    args.push(format!("{}@{}", user, host));
    args
}

pub fn sudoers_file_path(user: &str) -> Result<String> {
    validate_sudo_user(user)?;
    Ok(format!("/etc/sudoers.d/devpod-{}", user))
}

pub fn sudoers_content(user: &str) -> Result<String> {
    validate_sudo_user(user)?;
    Ok(format!("{} ALL=(ALL) NOPASSWD:ALL\n", user))
}

pub fn sudoers_install_script(user: &str) -> Result<String> {
    let path = sudoers_file_path(user)?;
    let content = sudoers_content(user)?;

    Ok(format!(
        r#"set -e
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
printf '%s' '{content}' > "$tmp"
chmod 0440 "$tmp"
sudo visudo -cf "$tmp"
sudo install -o root -g root -m 0440 "$tmp" {path}
sudo -n true
"#
    ))
}

pub fn safe_prereq_install_script() -> &'static str {
    r#"set -e
if command -v apt-get >/dev/null 2>&1; then
  sudo apt-get update
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y curl unzip ca-certificates avahi-daemon
  if command -v systemctl >/dev/null 2>&1; then
    sudo systemctl enable --now avahi-daemon || true
  fi
  echo "Safe prerequisites installed"
else
  echo "No supported package manager detected; skipping package installation."
  echo "Install curl, unzip, ca-certificates, and avahi-daemon manually if this node needs them."
fi
"#
}

pub fn is_tailscale_ipv4(host: &str) -> bool {
    let Ok(IpAddr::V4(addr)) = host.parse::<IpAddr>() else {
        return false;
    };

    let [first, second, _, _] = addr.octets();
    first == 100 && (64..=127).contains(&second)
}

pub fn parse_tailscale_peer(status_json: &str, host: &str) -> Result<Option<TailscalePeerInfo>> {
    let status: TailscaleStatus = serde_json::from_str(status_json)
        .context("Failed to parse `tailscale status --json` output")?;

    let nodes = status
        .self_node
        .into_iter()
        .chain(status.peers.into_values());
    for node in nodes {
        if node.tailscale_ips.iter().any(|ip| ip == host) {
            let name = node
                .dns_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .or(node
                    .host_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty()))
                .unwrap_or(host)
                .trim_end_matches('.')
                .to_string();

            return Ok(Some(TailscalePeerInfo {
                name,
                online: node.online,
                addresses: node.tailscale_ips,
            }));
        }
    }

    Ok(None)
}

pub fn classify_ssh_failure(
    output: &str,
    tailscale_peer: Option<&TailscalePeerInfo>,
) -> SshFailureDiagnosis {
    let lower = output.to_ascii_lowercase();
    let kind = if lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("kex_exchange_identification")
    {
        SshFailureKind::ConnectionRejectedBeforeAuth
    } else if lower.contains("permission denied") {
        SshFailureKind::PermissionDenied
    } else if lower.contains("operation timed out")
        || lower.contains("connection timed out")
        || lower.contains("no route to host")
        || lower.contains("could not resolve hostname")
    {
        SshFailureKind::TimeoutOrNoRoute
    } else if lower.contains("ssh-add -l")
        || lower.contains("the agent has no identities")
        || lower.contains("no identities")
        || lower.contains("could not open a connection to your authentication agent")
    {
        SshFailureKind::MissingIdentities
    } else {
        SshFailureKind::Unknown
    };

    let tailscale_context = tailscale_peer
        .map(|peer| {
            let online = match peer.online {
                Some(true) => "online",
                Some(false) => "offline",
                None => "visible",
            };
            format!(" Tailscale reports '{}' as {}.", peer.name, online)
        })
        .unwrap_or_default();

    let summary = match kind {
        SshFailureKind::ConnectionRejectedBeforeAuth => format!(
            "SSH was rejected before authentication.{} `ssh-copy-id` cannot fix this until the target accepts an SSH transport. If this is Tailscale SSH, verify the destination node has Tailscale SSH enabled and that the `ssh` ACL matches the actual source/destination identities, such as `autogroup:tagged` or the explicit node tags. Tailscale `grants` only allow network traffic; they do not enable Tailscale SSH. If this is normal OpenSSH, ensure sshd is listening on the tailnet IP.",
            tailscale_context
        ),
        SshFailureKind::PermissionDenied => format!(
            "SSH reached authentication but was denied.{} Check the configured user, accepted keys, and whether password login is allowed for first contact.",
            tailscale_context
        ),
        SshFailureKind::TimeoutOrNoRoute => format!(
            "SSH could not reach the host.{} Check Tailscale connectivity, routing, and whether port 22 is reachable on the target.",
            tailscale_context
        ),
        SshFailureKind::MissingIdentities => {
            "No SSH identities were available from the agent. Pass `--identity <path-to-public-key>` or add a key with `ssh-add`.".to_string()
        }
        SshFailureKind::Unknown => format!(
            "SSH failed for an unclassified reason.{} Re-run with manual `ssh -vvv` to inspect the transport/auth failure.",
            tailscale_context
        ),
    };

    SshFailureDiagnosis { kind, summary }
}

pub fn ssh_copy_id_failure_message(
    user: &str,
    host: &str,
    diagnosis: &SshFailureDiagnosis,
) -> String {
    let next_step = match diagnosis.kind {
        SshFailureKind::ConnectionRejectedBeforeAuth => format!(
            "Next checks: run `ssh -vvv {}@{} true` locally. If it still resets before auth, use node console access and run `sudo tailscale status`, `sudo tailscale up --ssh`, `sudo systemctl status ssh`, and `sudo ss -ltnp | grep ':22'`.",
            user, host
        ),
        SshFailureKind::PermissionDenied => format!(
            "Next checks: verify the `{}` account exists on the node and that first-contact password or key login is allowed.",
            user
        ),
        SshFailureKind::TimeoutOrNoRoute => {
            "Next checks: verify local Tailscale is connected, the target device is online, and port 22 is reachable over the selected address.".to_string()
        }
        SshFailureKind::MissingIdentities => {
            "Next checks: add a key with `ssh-add` or pass `--identity <path-to-public-key>`.".to_string()
        }
        SshFailureKind::Unknown => format!(
            "Next checks: run `ssh -vvv {}@{} true` locally and inspect the final transport/auth error.",
            user, host
        ),
    };

    format!(
        "ssh-copy-id failed for {}: {} {}",
        host, diagnosis.summary, next_step
    )
}

pub fn tailscale_command_candidates(args: &[&str]) -> Vec<TailscaleCommand> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();

    vec![
        TailscaleCommand {
            program: "tailscale".to_string(),
            args: args.clone(),
        },
        TailscaleCommand {
            program: "/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string(),
            args,
        },
    ]
}

pub fn tailscale_status_command_candidates() -> Vec<TailscaleCommand> {
    tailscale_command_candidates(&["status", "--json"])
}

pub fn resolve_init_nodes_plan(
    config: &DevpodConfig,
    env_name: &str,
    node_refs: &[String],
) -> Result<InitNodesPlan> {
    let cluster = config.get_cluster(env_name).with_context(|| {
        format!(
            "Environment '{}' not found in config. Run 'devpod context list' to see available contexts.",
            env_name
        )
    })?;

    if cluster.provider != "k3s" {
        anyhow::bail!(
            "init-nodes only supports remote k3s environments. '{}' uses provider '{}'",
            env_name,
            cluster.provider
        );
    }

    if cluster.nodes.is_empty() {
        anyhow::bail!("Environment '{}' has no nodes configured", env_name);
    }

    let selected_nodes = select_nodes(&cluster.nodes, node_refs)?;
    let mut targets = Vec::new();

    for node in selected_nodes {
        let host = node.bootstrap_address().with_context(|| {
            format!(
                "Node '{}' is missing bootstrap_address/address",
                node.stable_name()
            )
        })?;

        targets.push(InitNodeTarget {
            label: node.stable_name(),
            host: host.to_string(),
            role: node.role.clone(),
        });
    }

    Ok(InitNodesPlan {
        env_name: env_name.to_string(),
        user: cluster.user.as_deref().unwrap_or("root").to_string(),
        targets,
    })
}

pub async fn run_init_nodes(
    config: &DevpodConfig,
    env_name: &str,
    node_refs: &[String],
    identity: Option<&PathBuf>,
) -> Result<()> {
    let plan = resolve_init_nodes_plan(config, env_name, node_refs)?;
    let identity = identity.map(PathBuf::as_path);

    println!(
        "{} Initializing {} node(s) for '{}' as '{}'...",
        "->".blue().bold(),
        plan.targets.len(),
        plan.env_name.cyan(),
        plan.user.cyan()
    );

    let mut outcomes = Vec::new();

    for target in &plan.targets {
        println!(
            "\n{} Initializing {} ({}, {})",
            "->".blue(),
            target.label.cyan(),
            target.role,
            target.host
        );

        match initialize_node(&plan.user, target, identity).await {
            Ok(()) => {
                println!("{} {} initialized", "OK".green(), target.label);
                outcomes.push(InitNodeOutcome {
                    label: target.label.clone(),
                    host: target.host.clone(),
                    success: true,
                    message: "initialized".to_string(),
                });
            }
            Err(error) => {
                println!("{} {} failed: {}", "FAILED".red(), target.label, error);
                outcomes.push(InitNodeOutcome {
                    label: target.label.clone(),
                    host: target.host.clone(),
                    success: false,
                    message: error.to_string(),
                });
            }
        }
    }

    print_summary(&outcomes);

    let failures = outcomes.iter().filter(|outcome| !outcome.success).count();
    if failures > 0 {
        anyhow::bail!("{} node(s) failed initialization", failures);
    }

    Ok(())
}

async fn initialize_node(
    user: &str,
    target: &InitNodeTarget,
    identity: Option<&Path>,
) -> Result<()> {
    let tailscale_peer = tailscale_preflight(&target.host).await;

    if RemoteExecutor::can_connect(&target.host, user).await {
        println!("   {} SSH key access already works", "OK".green());
    } else {
        println!(
            "   {} Copying SSH key to {}@{}",
            "->".blue(),
            user,
            target.host
        );
        let args = ssh_copy_id_args(user, &target.host, identity);
        if let Err(error) = RemoteExecutor::run_interactive("ssh-copy-id", &args).await {
            let diagnosis =
                diagnose_ssh_copy_id_failure(user, &target.host, tailscale_peer.as_ref(), &error)
                    .await;
            anyhow::bail!(
                "{}",
                ssh_copy_id_failure_message(user, &target.host, &diagnosis)
            );
        }

        if !RemoteExecutor::can_connect(&target.host, user).await {
            anyhow::bail!("SSH key verification failed after ssh-copy-id");
        }
        println!("   {} SSH key access verified", "OK".green());
    }

    println!("   {} Configuring passwordless sudo", "->".blue());
    let sudoers_script = sudoers_install_script(user)?;
    RemoteExecutor::ssh_interactive(&target.host, user, &sudoers_script)
        .await
        .with_context(|| format!("Failed to configure sudoers on {}", target.host))?;
    RemoteExecutor::execute(&target.host, user, "sudo -n true")
        .await
        .with_context(|| format!("Passwordless sudo verification failed on {}", target.host))?;
    println!("   {} Passwordless sudo verified", "OK".green());

    println!("   {} Installing safe prerequisites", "->".blue());
    RemoteExecutor::ssh_interactive(&target.host, user, safe_prereq_install_script())
        .await
        .with_context(|| format!("Failed to install safe prerequisites on {}", target.host))?;

    Ok(())
}

async fn tailscale_preflight(host: &str) -> Option<TailscalePeerInfo> {
    if !is_tailscale_ipv4(host) {
        return None;
    }

    match read_tailscale_peer(host).await {
        Ok(Some(peer)) => {
            let online = match peer.online {
                Some(true) => "online",
                Some(false) => "offline",
                None => "visible",
            };
            println!(
                "   {} Tailscale sees '{}' as {} ({})",
                "OK".green(),
                peer.name,
                online,
                peer.addresses.join(", ")
            );
            Some(peer)
        }
        Ok(None) => {
            println!(
                "   {} {} is a Tailscale IP but was not found in local `tailscale status --json`",
                "!".yellow(),
                host
            );
            None
        }
        Err(error) => {
            println!(
                "   {} Could not inspect local Tailscale status: {}",
                "!".yellow(),
                error
            );
            None
        }
    }
}

async fn read_tailscale_peer(host: &str) -> Result<Option<TailscalePeerInfo>> {
    let mut failures = Vec::new();

    for candidate in tailscale_status_command_candidates() {
        let command_display = format!("{} {}", candidate.program, candidate.args.join(" "));
        let output = Command::new(&candidate.program)
            .args(&candidate.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return parse_tailscale_peer(&stdout, host);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = if !stderr.trim().is_empty() {
                    stderr.trim().to_string()
                } else {
                    stdout.trim().to_string()
                };
                failures.push(format!("{} exited with error: {}", command_display, detail));
            }
            Err(error) => {
                failures.push(format!("{} failed to spawn: {}", command_display, error));
            }
        }
    }

    anyhow::bail!(
        "Could not run local Tailscale status. Tried: {}",
        failures.join("; ")
    );
}

async fn diagnose_ssh_copy_id_failure(
    user: &str,
    host: &str,
    tailscale_peer: Option<&TailscalePeerInfo>,
    copy_id_error: &anyhow::Error,
) -> SshFailureDiagnosis {
    let diagnostic_output = match run_ssh_diagnostic(user, host).await {
        Ok(output) if !output.trim().is_empty() => output,
        Ok(_) => copy_id_error.to_string(),
        Err(error) => format!("{} {}", copy_id_error, error),
    };

    classify_ssh_failure(&diagnostic_output, tailscale_peer)
}

async fn run_ssh_diagnostic(user: &str, host: &str) -> Result<String> {
    let target = format!("{}@{}", user, host);
    let output = Command::new("ssh")
        .args([
            "-vvv",
            "-o",
            "BatchMode=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ConnectTimeout=10",
            &target,
            "true",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("Failed to run SSH diagnostic for {}", target))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!("{}\n{}", stdout, stderr))
}

fn select_nodes<'a>(
    nodes: &'a [RemoteNodeConfig],
    node_refs: &[String],
) -> Result<Vec<&'a RemoteNodeConfig>> {
    if node_refs.is_empty() {
        return Ok(nodes.iter().collect());
    }

    let mut selected = Vec::new();
    let mut missing = Vec::new();

    for node_ref in node_refs {
        if let Some(node) = nodes.iter().find(|node| node.matches_node_ref(node_ref)) {
            let node_key = node.bootstrap_address().unwrap_or("").to_string();
            let already_selected = selected.iter().any(|selected_node: &&RemoteNodeConfig| {
                selected_node.stable_name() == node.stable_name()
                    && selected_node.bootstrap_address().unwrap_or("") == node_key
            });

            if !already_selected {
                selected.push(node);
            }
        } else {
            missing.push(node_ref.clone());
        }
    }

    if !missing.is_empty() {
        anyhow::bail!(
            "Unknown node ref(s): {}. Valid node refs: {}",
            missing.join(", "),
            valid_node_refs(nodes).join(", ")
        );
    }

    Ok(selected)
}

fn valid_node_refs(nodes: &[RemoteNodeConfig]) -> Vec<String> {
    let mut refs = Vec::new();

    for node in nodes {
        refs.push(node.stable_name());
        if let Some(name) = node.name.as_deref() {
            if !name.trim().is_empty() {
                refs.push(name.to_string());
            }
        }
        if let Some(address) = node.bootstrap_address() {
            refs.push(address.to_string());
        }
    }

    refs.sort();
    refs.dedup();
    refs
}

fn validate_sudo_user(user: &str) -> Result<()> {
    let valid = !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'));

    if !valid {
        anyhow::bail!(
            "Refusing to generate sudoers config for unsafe username '{}'",
            user
        );
    }

    Ok(())
}

fn print_summary(outcomes: &[InitNodeOutcome]) {
    println!("\n=== INIT NODES SUMMARY ===");
    for outcome in outcomes {
        let status = if outcome.success {
            "OK".green()
        } else {
            "FAILED".red()
        };
        println!(
            "  {} {} ({}) - {}",
            status, outcome.label, outcome.host, outcome.message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_ssh_failure, is_tailscale_ipv4, parse_tailscale_peer, resolve_init_nodes_plan,
        safe_prereq_install_script, ssh_copy_id_args, ssh_copy_id_failure_message, sudoers_content,
        sudoers_file_path, sudoers_install_script, tailscale_status_command_candidates,
        SshFailureDiagnosis, SshFailureKind, TailscalePeerInfo,
    };
    use crate::config::{
        ClusterAccessConfig, ClusterDefinition, DeploymentConfig, DevpodConfig,
        InfrastructureConfig, NetworkConfig, ProjectConfig, RegistryConfig, RemoteNodeConfig,
        SecretsConfig, TailscaleConfig,
    };
    use std::collections::HashMap;
    use std::path::Path;

    fn test_config(clusters: HashMap<String, ClusterDefinition>) -> DevpodConfig {
        DevpodConfig {
            schema_version: 1,
            project: ProjectConfig {
                name: "edge-app".to_string(),
            },
            provider: None,
            cluster: clusters,
            cluster_defaults: None,
            registry: RegistryConfig::default(),
            infrastructure: InfrastructureConfig {
                persistent_storage_enabled: false,
                data_mount_path: "/tmp/devpod-test".to_string(),
            },
            deployment: DeploymentConfig {
                tool: "sailr".to_string(),
                environment: "test".to_string(),
            },
            network: NetworkConfig::default(),
            secrets: SecretsConfig::default(),
        }
    }

    fn remote_cluster() -> ClusterDefinition {
        ClusterDefinition {
            provider: "k3s".to_string(),
            connection: Some("ssh".to_string()),
            user: Some("pi".to_string()),
            nodes: vec![
                RemoteNodeConfig {
                    role: "server".to_string(),
                    name: Some("control-1".to_string()),
                    bootstrap_address: Some("192.168.1.10".to_string()),
                    address: None,
                    runtime: "containerd".to_string(),
                    labels: HashMap::new(),
                },
                RemoteNodeConfig {
                    role: "agent".to_string(),
                    name: Some("worker-1".to_string()),
                    bootstrap_address: Some("192.168.1.11".to_string()),
                    address: None,
                    runtime: "containerd".to_string(),
                    labels: HashMap::new(),
                },
            ],
            datastore_endpoint: None,
            access: ClusterAccessConfig::default(),
            tailscale: TailscaleConfig::default(),
        }
    }

    fn k3d_cluster() -> ClusterDefinition {
        ClusterDefinition {
            provider: "k3d".to_string(),
            connection: None,
            user: None,
            nodes: Vec::new(),
            datastore_endpoint: None,
            access: ClusterAccessConfig::default(),
            tailscale: TailscaleConfig::default(),
        }
    }

    #[test]
    fn init_plan_selects_all_nodes_by_default() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);

        let plan = resolve_init_nodes_plan(&config, "edge", &[]).unwrap();

        assert_eq!(plan.user, "pi");
        assert_eq!(plan.targets.len(), 2);
        assert_eq!(plan.targets[0].label, "control-1");
        assert_eq!(plan.targets[1].label, "worker-1");
    }

    #[test]
    fn init_plan_filters_repeated_node_refs() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);

        let plan = resolve_init_nodes_plan(
            &config,
            "edge",
            &[
                "worker-1".to_string(),
                "192.168.1.11".to_string(),
                "worker-1".to_string(),
            ],
        )
        .unwrap();

        assert_eq!(plan.targets.len(), 1);
        assert_eq!(plan.targets[0].label, "worker-1");
        assert_eq!(plan.targets[0].host, "192.168.1.11");
    }

    #[test]
    fn init_plan_unknown_node_ref_fails_with_valid_refs() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);

        let error = resolve_init_nodes_plan(&config, "edge", &["missing".to_string()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("Unknown node ref"));
        assert!(error.contains("control-1"));
        assert!(error.contains("worker-1"));
    }

    #[test]
    fn init_plan_rejects_local_provider() {
        let mut clusters = HashMap::new();
        clusters.insert("dev".to_string(), k3d_cluster());
        let config = test_config(clusters);

        let error = resolve_init_nodes_plan(&config, "dev", &[])
            .unwrap_err()
            .to_string();

        assert!(error.contains("only supports remote k3s"));
    }

    #[test]
    fn init_plan_rejects_missing_node_address() {
        let mut cluster = remote_cluster();
        cluster.nodes[0].bootstrap_address = None;
        cluster.nodes[0].address = None;
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), cluster);
        let config = test_config(clusters);

        let error = resolve_init_nodes_plan(&config, "edge", &["control-1".to_string()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing bootstrap_address/address"));
    }

    #[test]
    fn ssh_copy_id_args_include_identity_when_provided() {
        assert_eq!(
            ssh_copy_id_args(
                "pi",
                "192.168.1.10",
                Some(Path::new("/Users/me/.ssh/id_ed25519.pub"))
            ),
            vec![
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                "-o".to_string(),
                "ConnectTimeout=10".to_string(),
                "-i".to_string(),
                "/Users/me/.ssh/id_ed25519.pub".to_string(),
                "pi@192.168.1.10".to_string()
            ]
        );
    }

    #[test]
    fn ssh_copy_id_args_without_identity_use_default_key_discovery() {
        let args = ssh_copy_id_args("pi", "192.168.1.10", None);

        assert!(!args.contains(&"-i".to_string()));
        assert_eq!(args.last().unwrap(), "pi@192.168.1.10");
    }

    #[test]
    fn sudoers_helpers_reject_unsafe_users() {
        assert!(sudoers_file_path("bad/user").is_err());
        assert!(sudoers_content("bad user").is_err());
        assert!(sudoers_content("-bad").is_err());
    }

    #[test]
    fn sudoers_helpers_emit_dedicated_file_and_content() {
        assert_eq!(sudoers_file_path("pi").unwrap(), "/etc/sudoers.d/devpod-pi");
        assert_eq!(
            sudoers_content("pi").unwrap(),
            "pi ALL=(ALL) NOPASSWD:ALL\n"
        );
        assert!(sudoers_install_script("pi").unwrap().contains("visudo -cf"));
    }

    #[test]
    fn safe_prereq_script_is_apt_only_and_idempotent() {
        let script = safe_prereq_install_script();

        assert!(script.contains("command -v apt-get"));
        assert!(script.contains("apt-get install -y"));
        assert!(script.contains("curl unzip ca-certificates avahi-daemon"));
        assert!(script.contains("skipping package installation"));
    }

    #[test]
    fn tailscale_ipv4_detection_matches_tailnet_range() {
        assert!(is_tailscale_ipv4("100.96.48.109"));
        assert!(is_tailscale_ipv4("100.127.107.2"));
        assert!(!is_tailscale_ipv4("100.128.0.1"));
        assert!(!is_tailscale_ipv4("192.168.1.10"));
        assert!(!is_tailscale_ipv4("alpha-dev-edge"));
    }

    #[test]
    fn tailscale_status_candidates_include_macos_app_binary() {
        let candidates = tailscale_status_command_candidates();

        assert!(candidates.iter().any(|candidate| {
            candidate.program == "tailscale"
                && candidate.args == vec!["status".to_string(), "--json".to_string()]
        }));
        assert!(candidates.iter().any(|candidate| {
            candidate.program == "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
                && candidate.args == vec!["status".to_string(), "--json".to_string()]
        }));
    }

    #[test]
    fn parses_tailscale_peer_from_status_json() {
        let status = r#"
{
  "Self": {
    "HostName": "adrifts-macbook-pro",
    "DNSName": "adrifts-macbook-pro.example.ts.net.",
    "TailscaleIPs": ["100.124.241.88"],
    "Online": true
  },
  "Peer": {
    "nodekey:abc": {
      "HostName": "alpha-dev-edge-16791a",
      "DNSName": "alpha-dev-edge-16791a.example.ts.net.",
      "TailscaleIPs": ["100.96.48.109"],
      "Online": true
    }
  }
}
"#;

        let peer = parse_tailscale_peer(status, "100.96.48.109")
            .unwrap()
            .unwrap();

        assert_eq!(peer.name, "alpha-dev-edge-16791a.example.ts.net");
        assert_eq!(peer.online, Some(true));
        assert_eq!(peer.addresses, vec!["100.96.48.109".to_string()]);
    }

    #[test]
    fn parses_missing_tailscale_peer_as_none() {
        let status = r#"{ "Peer": {} }"#;

        assert!(parse_tailscale_peer(status, "100.96.48.109")
            .unwrap()
            .is_none());
    }

    #[test]
    fn classifies_connection_reset_as_pre_auth_rejection() {
        let diagnosis = classify_ssh_failure(
            "kex_exchange_identification: read: Connection reset by peer",
            None,
        );

        assert_eq!(diagnosis.kind, SshFailureKind::ConnectionRejectedBeforeAuth);
        assert!(diagnosis.summary.contains("rejected before authentication"));
    }

    #[test]
    fn classifies_connection_closed_as_pre_auth_rejection() {
        let diagnosis = classify_ssh_failure("Connection closed by 100.96.48.109 port 22", None);

        assert_eq!(diagnosis.kind, SshFailureKind::ConnectionRejectedBeforeAuth);
    }

    #[test]
    fn classifies_permission_denied() {
        let diagnosis = classify_ssh_failure("Permission denied (publickey,password).", None);

        assert_eq!(diagnosis.kind, SshFailureKind::PermissionDenied);
        assert!(diagnosis.summary.contains("configured user"));
    }

    #[test]
    fn classifies_timeout_or_no_route() {
        let diagnosis = classify_ssh_failure(
            "ssh: connect to host 100.96.48.109 port 22: Operation timed out",
            None,
        );

        assert_eq!(diagnosis.kind, SshFailureKind::TimeoutOrNoRoute);
        assert!(diagnosis.summary.contains("could not reach"));
    }

    #[test]
    fn classifies_missing_agent_identities() {
        let diagnosis = classify_ssh_failure("The agent has no identities.", None);

        assert_eq!(diagnosis.kind, SshFailureKind::MissingIdentities);
        assert!(diagnosis
            .summary
            .contains("--identity <path-to-public-key>"));
    }

    #[test]
    fn tailscale_visible_connection_reset_mentions_online_device_and_ssh_block() {
        let peer = TailscalePeerInfo {
            name: "alpha-dev-edge-16791a.example.ts.net".to_string(),
            online: Some(true),
            addresses: vec!["100.96.48.109".to_string()],
        };

        let diagnosis = classify_ssh_failure(
            "kex_exchange_identification: read: Connection reset by peer",
            Some(&peer),
        );

        assert_eq!(diagnosis.kind, SshFailureKind::ConnectionRejectedBeforeAuth);
        assert!(diagnosis.summary.contains("Tailscale reports"));
        assert!(diagnosis.summary.contains("online"));
        assert!(diagnosis.summary.contains("Tailscale `grants`"));
        assert!(diagnosis
            .summary
            .contains("destination node has Tailscale SSH enabled"));
        assert!(diagnosis.summary.contains("autogroup:tagged"));
        assert!(diagnosis.summary.contains("sshd is listening"));
    }

    #[test]
    fn ssh_copy_id_summary_includes_diagnostic_text() {
        let diagnosis = SshFailureDiagnosis {
            kind: SshFailureKind::ConnectionRejectedBeforeAuth,
            summary: "SSH was rejected before authentication.".to_string(),
        };
        let message = ssh_copy_id_failure_message("dev", "100.96.48.109", &diagnosis);

        assert!(message.contains("ssh-copy-id failed for 100.96.48.109"));
        assert!(message.contains("SSH was rejected before authentication"));
        assert!(message.contains("ssh -vvv dev@100.96.48.109 true"));
        assert!(message.contains("sudo tailscale up --ssh"));
    }
}
