use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod builder;
mod config;
mod executor;
mod orchestrator; // Add executor module
mod util;

use builder::Builder;
use config::{ClusterDefinition, DevpodConfig, RemoteNodeConfig};
use executor::RemoteExecutor;
use orchestrator::get_manager; // Use RemoteExecutor

/// Devpod: Edge Orchestrator
#[derive(Parser)]
#[command(name = "devpod")]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, global = true, default_value = "devpod.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

fn node_connection_candidates(cluster: &ClusterDefinition, node: &RemoteNodeConfig) -> Vec<String> {
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

    candidates.dedup();
    candidates
}

async fn resolve_node_host(
    cluster: &ClusterDefinition,
    node: &RemoteNodeConfig,
    user: &str,
) -> Option<String> {
    let candidates = node_connection_candidates(cluster, node);
    let candidate_refs: Vec<_> = candidates.iter().map(String::as_str).collect();

    RemoteExecutor::first_reachable(candidate_refs.iter().copied(), user)
        .await
        .or_else(|| node.bootstrap_address().map(str::to_string))
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new project
    Init {
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Provision cluster, build, and deploy
    Up {
        /// Target environment (matches key in [cluster.<env>])
        #[arg(long)]
        env: Option<String>,
    },

    /// Build and deploy (skip provision check)
    Build {
        /// Target environment
        #[arg(long)]
        env: Option<String>,
    },

    Down {
        /// Target environment
        #[arg(long)]
        env: Option<String>,
    },

    /// Run setup script on all nodes (cgroups, deps, reboot)
    Setup {
        /// Target environment
        #[arg(long)]
        env: String,
    },

    /// Check status
    Status {
        /// Target environment
        #[arg(long)]
        env: Option<String>,
    },

    /// List physical status of all nodes in an environment
    Nodes {
        /// Target environment
        #[arg(long)]
        env: String,
    },

    /// SSH into a cluster node
    Ssh {
        /// Target environment
        #[arg(long)]
        env: String,

        /// Node ID (index or IP/Host)
        node: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "devpod=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // Handle Init
    if let Commands::Init { name } = &cli.command {
        let project_name = name
            .clone()
            .unwrap_or_else(|| "my-edge-project".to_string());

        if std::path::Path::new("devpod.toml").exists() {
            println!("{} devpod.toml already exists.", "!".yellow());
            return Ok(());
        }

        let toml_content = format!(
            r#"[project]
name = "{}"

# Local Development Cluster
[cluster.dev]
provider = "k3d"

# Example Remote Environment
[cluster.production-van]
provider = "k3s"
connection = "ssh"
user = "admin"
[[cluster.production-van.nodes]]
role = "server"
name = "control-1"
bootstrap_address = "192.168.1.10"
runtime = "containerd"

[cluster.production-van.access]
mode = "dual"
primary = "tailscale"
lan_domain = "local"

[cluster.production-van.tailscale]
enabled = true
tailnet_domain = "example.ts.net"
auth_key_env = "TAILSCALE_AUTH_KEY"
api_key_env = "TAILSCALE_API_KEY"
tags = ["tag:k3s"]
ssh = true

[infrastructure]
persistent_storage_enabled = true
data_mount_path = "/var/lib/devpod/storage"

[deployment]
tool = "sailr"
environment = "edge-production"

[network]
expose = [
  {{ host = 8080, container = 80, protocol = "HTTP" }}
]
"#,
            project_name
        );

        std::fs::write("devpod.toml", toml_content)?;

        println!(
            "{} Initialized new project '{}'",
            "OK".green(),
            project_name
        );
        return Ok(());
    }

    // Load config
    let config = match DevpodConfig::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Failed to load config: {}", "!".yellow(), e);
            std::process::exit(1);
        }
    };

    match cli.command {
        Commands::Up { env } => {
            let manager = get_manager(&config, env.as_deref());

            // 1. Provision Cluster
            manager.up(&config).await?;

            // 2. Build
            Builder::build(&config).await?;

            // 3. Apply Manifests
            let manifest_path = Builder::get_manifest_path(&config);
            if manifest_path.exists() {
                manager.sync_secrets(&config).await?; // Call sync_secrets before apply
                manager.apply_manifests(&config, manifest_path).await?;
            } else {
                println!(
                    "{} No manifests found at {:?}, skipping apply.",
                    "!".yellow(),
                    manifest_path
                );
            }

            println!("{} Environment is up and running.", "OK".green());
        }
        Commands::Build { env } => {
            let manager = get_manager(&config, env.as_deref());

            // Just build & deploy
            Builder::build(&config).await?;
            let manifest_path = Builder::get_manifest_path(&config);
            if manifest_path.exists() {
                manager.apply_manifests(&config, manifest_path).await?;
            }
            println!("{} Build and deploy complete.", "OK".green());
        }
        Commands::Down { env } => {
            let manager = get_manager(&config, env.as_deref());
            manager.down(&config).await?;
            println!("{} Environment stopped.", "OK".green());
        }
        Commands::Status { env: _ } => {
            println!("{} Checking status...", "->".blue());

            // Check node status using kubectl
            let _ = tokio::process::Command::new("kubectl")
                .arg("get")
                .arg("nodes")
                .status()
                .await;
        }
        Commands::Nodes { env } => {
            if let Some(cluster) = config.get_cluster(&env) {
                let user = cluster.user.as_deref().unwrap_or("root");
                println!("{} Checking nodes for '{}'...", "->".blue(), env);
                for node in &cluster.nodes {
                    let label = node
                        .bootstrap_address()
                        .map(str::to_string)
                        .unwrap_or_else(|| node.stable_name());
                    print!("  Node {} ({}) ... ", label, node.role);
                    let target = resolve_node_host(cluster, node, user).await;
                    match target {
                        Some(host) => match RemoteExecutor::execute(&host, user, "uptime").await {
                            Ok(out) => println!("{} (up: {})", "OK".green(), out.trim()),
                            Err(_) => println!("{}", "UNREACHABLE".red()),
                        },
                        None => println!("{}", "UNCONFIGURED".red()),
                    }
                }
            } else {
                println!("{} Environment '{}' not found in config", "!".red(), env);
            }
        }
        Commands::Ssh { env, node } => {
            if let Some(cluster) = config.get_cluster(&env) {
                let target_node = cluster.nodes.iter().find(|n| n.matches_node_ref(&node));

                if let Some(n) = target_node {
                    let user = cluster.user.as_deref().unwrap_or("root");
                    let host = resolve_node_host(cluster, n, user)
                        .await
                        .context("No reachable SSH target found for node")?;
                    println!("{} Connecting to {}...", "->".blue(), host);
                    RemoteExecutor::shell(&host, user).await?;
                } else {
                    // Try using 'node' as index if integer?
                    println!("{} Node '{}' not found in cluster config", "!".red(), node);
                }
            } else {
                println!("{} Environment '{}' not found", "!".red(), env);
            }
        }
        Commands::Setup { env } => {
            if let Some(cluster) = config.get_cluster(&env) {
                let user = cluster.user.as_deref().unwrap_or("root");
                println!("{} Setting up nodes for '{}'...", "->".blue().bold(), env);

                for node in &cluster.nodes {
                    let Some(host) = node.bootstrap_address() else {
                        println!(
                            "{} Skipping node '{}' because it has no bootstrap address",
                            "!".yellow(),
                            node.stable_name()
                        );
                        continue;
                    };

                    println!("{} Configuring Node: {}", "->".blue(), host);

                    // 1. Enable cgroups for Docker/K3s compatibility
                    let cmd_cgroups = "if ! grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt; then 
                        sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/firmware/cmdline.txt
                        echo 'Updated /boot/firmware/cmdline.txt'
                    elif ! grep -q 'cgroup_memory=1' /boot/cmdline.txt; then
                        sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/cmdline.txt
                         echo 'Updated /boot/cmdline.txt'
                    else
                        echo 'Cgroups already configured'
                    fi";

                    match RemoteExecutor::execute(host, user, cmd_cgroups).await {
                        Ok(out) => println!("   {} Cgroups: {}", "OK".green(), out.trim()),
                        Err(e) => println!(
                            "{} Failed to configure cgroups on {}: {}",
                            "!".red(),
                            host,
                            e
                        ),
                    }

                    // 2. Install Dependencies (curl, etc)
                    println!("   Installing dependencies...");
                    let cmd_deps = "sudo apt-get update && sudo apt-get install -y curl unzip";
                    if let Err(e) = RemoteExecutor::execute(host, user, cmd_deps).await {
                        println!("{} Failed to install deps on {}: {}", "!".yellow(), host, e);
                    } else {
                        println!("   {} Dependencies installed", "OK".green());
                    }

                    // 3. Reboot
                    println!("   {} Rebooting node {}...", "->".yellow(), host);
                    // Node reboots; expected to fail when connection drops
                    let _ = RemoteExecutor::execute(host, user, "sudo reboot").await;
                }

                println!(
                    "{} Setup command sent to all nodes. Please wait for them to reboot.",
                    "OK".green()
                );
            } else {
                println!("{} Environment '{}' not found in config", "!".red(), env);
            }
        }
        Commands::Init { .. } => unreachable!(),
    }

    Ok(())
}
