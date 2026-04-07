use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod builder;
mod config;
mod error;
mod executor;
mod orchestrator; // Add executor module
mod util;

use builder::Builder;
use config::DevpodConfig;
use executor::{Executor, RemoteExecutor};
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
address = "192.168.1.10"
runtime = "containerd"

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
                manager.apply_manifests(manifest_path).await?;
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
                manager.apply_manifests(manifest_path).await?;
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
                    print!("  Node {} ({}) ... ", node.address, node.role);
                    // Simple check: uptime
                    let executor = RemoteExecutor;
                    match executor.execute(&node.address, user, "uptime").await {
                        Ok(out) => println!("{} (up: {})", "OK".green(), out.trim()),
                        Err(_) => println!("{}", "UNREACHABLE".red()),
                    }
                }
            } else {
                println!("{} Environment '{}' not found in config", "!".red(), env);
            }
        }
        Commands::Ssh { env, node } => {
            if let Some(cluster) = config.get_cluster(&env) {
                // Match node by address
                let target_node = cluster.nodes.iter().find(|n| n.address == node);

                if let Some(n) = target_node {
                    let user = cluster.user.as_deref().unwrap_or("root");
                    println!("{} Connecting to {}...", "->".blue(), n.address);
                    let executor = RemoteExecutor;
                    executor.shell(&n.address, user).await?;
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
                    println!("{} Configuring Node: {}", "->".blue(), node.address);

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

                    let executor = RemoteExecutor;
                    match executor.execute(&node.address, user, cmd_cgroups).await {
                        Ok(out) => println!("   {} Cgroups: {}", "OK".green(), out.trim()),
                        Err(e) => println!(
                            "{} Failed to configure cgroups on {}: {}",
                            "!".red(),
                            node.address,
                            e
                        ),
                    }

                    // 2. Install Dependencies (curl, etc)
                    println!("   Installing dependencies...");
                    let cmd_deps = "sudo apt-get update && sudo apt-get install -y curl unzip";
                    if let Err(e) = executor.execute(&node.address, user, cmd_deps).await {
                        println!(
                            "{} Failed to install deps on {}: {}",
                            "!".yellow(),
                            node.address,
                            e
                        );
                    } else {
                        println!("   {} Dependencies installed", "OK".green());
                    }

                    // 3. Reboot
                    println!("   {} Rebooting node {}...", "->".yellow(), node.address);
                    // Node reboots; expected to fail when connection drops
                    let _ = executor.execute(&node.address, user, "sudo reboot").await;
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
