use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod builder;
mod config;
mod orchestrator;

use builder::Builder;
use config::DevpodConfig;
use orchestrator::get_manager;

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
    Up,
    
    /// Build and deploy (skip provision check)
    Build,
    
    /// Stop the environment
    Down,
    
    /// Check status
    Status,
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

[provider]
# Defaults to auto-detect (k3d for Mac/Win, k3s for Linux)
type = "auto"

[registry]
enabled = true
port = 32000

[infrastructure]
# Ensure apps have a place to store data locally
persistent_storage_enabled = true
data_mount_path = "/var/lib/devpod/storage"

[deployment]
# Integration with your preferred build tool
tool = "sailr"
environment = "edge-production"

[network]
# Ports for drones to hit the ingest service
expose = [
  {{ host = 1883, container = 1883, protocol = "TCP" }}, 
  {{ host = 8080, container = 80, protocol = "HTTP" }}
]
"#, project_name);

        std::fs::write("devpod.toml", toml_content)?;
        
        println!("{} Initialized new project '{}'", "OK".green(), project_name);
        return Ok(());
    }

    // Load config for other commands
    let config = match DevpodConfig::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Failed to load config: {}", "!".yellow(), e);
            std::process::exit(1);
        }
    };

    let manager = get_manager(&config);

    match cli.command {
        Commands::Up => {
            // 1. Provision Cluster
            manager.up(&config).await?;
            
            // 2. Build
            Builder::build(&config).await?;
            
            // 3. Sync Images (Optional/Placeholder in current builder impl)
            let images = Builder::export_images(&config).await?;
            manager.sync_images(images).await?;
            
            // 4. Apply Manifests
            let manifest_path = Builder::get_manifest_path(&config);
            if manifest_path.exists() {
                 manager.apply_manifests(manifest_path).await?;
            } else {
                 println!("{} No manifests found at {:?}, skipping apply.", "!".yellow(), manifest_path);
            }
            
            println!("{} Environment is up and running.", "OK".green());
        },
        Commands::Build => {
             // Just build & deploy
             Builder::build(&config).await?;
             let manifest_path = Builder::get_manifest_path(&config);
             if manifest_path.exists() {
                 manager.apply_manifests(manifest_path).await?;
             }
             println!("{} Build and deploy complete.", "OK".green());
        },
        Commands::Down => {
            manager.down().await?;
            println!("{} Environment stopped.", "OK".green());
        },
        Commands::Status => {
            // Simple status check
            // In real impl, check k8s nodes/pods
             println!("{} Checking status...", "->".blue());
             // This method signature in trait doesn't return info yet, just bool?
             // Spec said: "Summarizes cluster health, node status, and pod readiness."
             // We'll just call kubectl for now as a quick implementation if cluster is up
             let status = tokio::process::Command::new("kubectl")
                .arg("get")
                .arg("nodes")
                .status()
                .await;
            
            if status.is_ok() && status.unwrap().success() {
                 println!("{} Cluster is responsive.", "OK".green());
                 let _ = tokio::process::Command::new("kubectl").arg("get").arg("pods").arg("-A").status().await;
            } else {
                 println!("{} Cluster is unreachable.", "!".red());
            }
        },
        Commands::Init { .. } => unreachable!(),
    }

    Ok(())
}
