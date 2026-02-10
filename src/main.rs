use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod builder;
mod config;
mod orchestrator;
mod executor; // Add executor module

use builder::Builder;
use config::DevpodConfig;
use orchestrator::get_manager;
use executor::RemoteExecutor; // Use RemoteExecutor

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
    
    /// Stop the environment
    Down,
    
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
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "devpod=info".into())))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    
    // Handle Init
    if let Commands::Init { name } = &cli.command {
        let project_name = name.clone().unwrap_or_else(|| "my-edge-project".to_string());
        
        if std::path::Path::new("devpod.toml").exists() {
            println!("{} devpod.toml already exists.", "!".yellow());
            return Ok(());
        }

        let toml_content = format!(r#"[project]
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
"#, project_name);

        std::fs::write("devpod.toml", toml_content)?;
        
        println!("{} Initialized new project '{}'", "OK".green(), project_name);
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
                 println!("{} No manifests found at {:?}, skipping apply.", "!".yellow(), manifest_path);
            }
            
            println!("{} Environment is up and running.", "OK".green());
        },
        Commands::Build { env } => {
             let manager = get_manager(&config, env.as_deref());
             
             // Just build & deploy
             Builder::build(&config).await?;
             let manifest_path = Builder::get_manifest_path(&config);
             if manifest_path.exists() {
                 manager.apply_manifests(manifest_path).await?;
             }
             println!("{} Build and deploy complete.", "OK".green());
        },
        Commands::Down => {
            // Default to local down for safety, unless we want to support remote down (dangerous?)
            let manager = get_manager(&config, None); 
            manager.down().await?;
            println!("{} Environment stopped.", "OK".green());
        },
        Commands::Status { env } => {
             // For remote, maybe check kubectl get nodes using context?
             println!("{} Checking status...", "->".blue());
             let context_arg = if let Some(e) = env {
                 format!("devpod-{}", e)
             } else {
                 // Default context check (local)
                 // This assumes current context is set correctly
                 "default".to_string() // or whatever current is
             };
             
             // Simplification: just run kubectl get nodes
             // If remote env specified, we might need to set KUBECONFIG or context if we switched it
             // But 'up' merges it.
             let _ = tokio::process::Command::new("kubectl")
                .arg("get")
                .arg("nodes")
                .status()
                .await;
        },
        Commands::Nodes { env } => {
            if let Some(cluster) = config.get_cluster(&env) {
                let user = cluster.user.as_deref().unwrap_or("root");
                println!("{} Checking nodes for '{}'...", "->".blue(), env);
                for node in &cluster.nodes {
                     print!("  Node {} ({}) ... ", node.address, node.role);
                     // Simple check: uptime
                     match RemoteExecutor::execute(&node.address, user, "uptime").await {
                         Ok(out) => println!("{} (up: {})", "OK".green(), out.trim()),
                         Err(_) => println!("{}", "UNREACHABLE".red()),
                     }
                }
            } else {
                println!("{} Environment '{}' not found in config", "!".red(), env);
            }
        },
        Commands::Ssh { env, node } => {
            if let Some(cluster) = config.get_cluster(&env) {
                 // Find node by address match or index?
                 // Let's assume 'node' arg is the address for simplicity or try to match name if we added name field
                 let target_node = cluster.nodes.iter().find(|n| n.address == node);
                 
                 if let Some(n) = target_node {
                     let user = cluster.user.as_deref().unwrap_or("root");
                     println!("{} Connecting to {}...", "->".blue(), n.address);
                     RemoteExecutor::shell(&n.address, user).await?;
                 } else {
                     // Try using 'node' as index if integer?
                     println!("{} Node '{}' not found in cluster config", "!".red(), node);
                 }
            } else {
                 println!("{} Environment '{}' not found", "!".red(), env);
            }
        }
        Commands::Init { .. } => unreachable!(),
    }

    Ok(())
}
