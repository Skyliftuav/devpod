use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use std::io::Write;

mod builder;
mod config;
mod executor;
mod orchestrator; // Add executor module
mod util;

use builder::Builder;
use config::{ClusterDefinition, DevpodConfig, RemoteNodeConfig};
use executor::RemoteExecutor;
use orchestrator::get_manager; // Use RemoteExecutor
use orchestrator::remote::RemoteManager;

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
enum ContextCommands {
    /// List all available contexts
    List,
    /// Set the active context
    Use {
        /// Context/environment name
        env: String,
    },
    /// Show current active context
    Show,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Migrate legacy single-file configuration to modular version 1 schema
    Migrate,
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

        /// Also purge Tailscale from remote nodes
        #[arg(long, default_value_t = false)]
        purge_tailscale: bool,
    },

    /// Refresh kubeconfig contexts for a remote environment
    SyncContext {
        /// Target environment
        #[arg(long)]
        env: Option<String>,
    },

    /// Run setup script on all nodes (cgroups, deps, reboot)
    Setup {
        /// Target environment
        #[arg(long)]
        env: Option<String>,
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
        env: Option<String>,
    },

    /// SSH into a cluster node
    Ssh {
        /// Target environment
        #[arg(long)]
        env: Option<String>,

        /// Node ID (index or IP/Host)
        node: String,
    },

    /// Manage environment contexts
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Diagnose cluster, config, and system dependencies
    Doctor {
        /// Target environment (defaults to active context)
        #[arg(long)]
        env: Option<String>,
    },

    /// Automatically repair diagnosed issues
    Repair {
        /// Target environment (defaults to active context)
        #[arg(long)]
        env: Option<String>,
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

    let state = config::DevpodState::load(&cli.config);

    let resolve_env = |env: Option<String>| -> Result<String> {
        if let Some(e) = env {
            Ok(e)
        } else if let Some(ref active) = state.active_environment {
            Ok(active.clone())
        } else {
            anyhow::bail!("No environment specified and no active context set. Use --env <env> or set an active context with 'devpod context use <env>'.")
        }
    };

    match cli.command {
        Commands::Up { env } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            let manager = get_manager(&config, resolved_env);

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
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            let manager = get_manager(&config, resolved_env);

            // Just build & deploy
            Builder::build(&config).await?;
            let manifest_path = Builder::get_manifest_path(&config);
            if manifest_path.exists() {
                manager.apply_manifests(&config, manifest_path).await?;
            }
            println!("{} Build and deploy complete.", "OK".green());
        }
        Commands::Down { env, purge_tailscale } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            let manager = get_manager(&config, resolved_env);
            manager.down(&config, purge_tailscale).await?;
            println!("{} Environment stopped.", "OK".green());
        }
        Commands::SyncContext { env } => {
            let env_name = resolve_env(env)?;
            let cluster = config.get_cluster(&env_name).with_context(|| {
                format!("Environment '{}' not found in config", env_name)
            })?;

            if cluster.provider != "k3s" {
                anyhow::bail!(
                    "sync-context only supports remote k3s environments. '{}' uses provider '{}'",
                    env_name,
                    cluster.provider
                );
            }

            let manager = RemoteManager::new(&env_name);
            manager.sync_context(&config).await?;
            println!("{} Kubeconfig context refreshed.", "OK".green());
        }
        Commands::Status { env } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            println!(
                "{} Checking status for {}...",
                "->".blue(),
                resolved_env.unwrap_or("local")
            );

            // Check node status using kubectl
            let _ = tokio::process::Command::new("kubectl")
                .arg("get")
                .arg("nodes")
                .status()
                .await;
        }
        Commands::Nodes { env } => {
            let env_name = resolve_env(env)?;
            if let Some(cluster) = config.get_cluster(&env_name) {
                let user = cluster.user.as_deref().unwrap_or("root");
                println!("{} Checking nodes for '{}'...", "->".blue(), env_name);
                for node in &cluster.nodes {
                    let label = node
                        .bootstrap_address()
                        .map(str::to_string)
                        .unwrap_or_else(|| node.stable_name());
                    print!("  Node {} ({}) ... ", label, node.role);
                    std::io::stdout().flush().unwrap();
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
                println!("{} Environment '{}' not found in config", "!".red(), env_name);
            }
        }
        Commands::Ssh { env, node } => {
            let env_name = resolve_env(env)?;
            if let Some(cluster) = config.get_cluster(&env_name) {
                let target_node = cluster.nodes.iter().find(|n| n.matches_node_ref(&node));

                if let Some(n) = target_node {
                    let user = cluster.user.as_deref().unwrap_or("root");
                    let host = resolve_node_host(cluster, n, user)
                        .await
                        .context("No reachable SSH target found for node")?;
                    println!("{} Connecting to {}...", "->".blue(), host);
                    RemoteExecutor::shell(&host, user).await?;
                } else {
                    println!("{} Node '{}' not found in cluster config", "!".red(), node);
                }
            } else {
                println!("{} Environment '{}' not found", "!".red(), env_name);
            }
        }
        Commands::Setup { env } => {
            let env_name = resolve_env(env)?;
            if let Some(cluster) = config.get_cluster(&env_name) {
                let user = cluster.user.as_deref().unwrap_or("root");
                println!("{} Setting up nodes for '{}'...", "->".blue().bold(), env_name);

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

                    let running_cgroups = format!("      * Configuring cgroups ... {}", "∨".blue());
                    let success_cgroups = format!("      * Configuring cgroups ... {}", "OK".green());
                    let failure_cgroups = format!("      * Configuring cgroups ... {}", "FAILED".red());
                    let _ = RemoteExecutor::execute_live(
                        host,
                        user,
                        cmd_cgroups,
                        &running_cgroups,
                        &success_cgroups,
                        &failure_cgroups,
                    )
                    .await;

                    // 2. Install Dependencies (curl, etc)
                    let cmd_deps = "sudo apt-get update && sudo apt-get install -y curl unzip";
                    let running_deps = format!("      * Installing dependencies ... {}", "∨".blue());
                    let success_deps = format!("      * Installing dependencies ... {}", "OK".green());
                    let failure_deps = format!("      * Installing dependencies ... {}", "FAILED".red());
                    let _ = RemoteExecutor::execute_live(
                        host,
                        user,
                        cmd_deps,
                        &running_deps,
                        &success_deps,
                        &failure_deps,
                    )
                    .await;

                    // 3. Reboot
                    println!("      * {} Rebooting node {}...", "->".yellow(), host);
                    let _ = RemoteExecutor::execute(host, user, "sudo reboot").await;
                }

                println!(
                    "{} Setup command sent to all nodes. Please wait for them to reboot.",
                    "OK".green()
                );
            } else {
                println!("{} Environment '{}' not found in config", "!".red(), env_name);
            }
        }
        Commands::Context { command } => {
            match command {
                ContextCommands::List => {
                    println!("Available contexts:");
                    for cluster_name in config.cluster.keys() {
                        let marker = if state.active_environment.as_ref() == Some(cluster_name) {
                            "* ".green().bold()
                        } else {
                            "  ".normal()
                        };
                        println!("{}{}", marker, cluster_name);
                    }
                }
                ContextCommands::Use { env } => {
                    if config.cluster.contains_key(&env) {
                        let mut state = state;
                        state.active_environment = Some(env.clone());
                        state.save(&cli.config)?;
                        println!("{} Switched to context '{}'", "OK".green(), env);
                    } else {
                        println!("{} Environment '{}' not found in config", "!".red(), env);
                        std::process::exit(1);
                    }
                }
                ContextCommands::Show => {
                    if let Some(ref env) = state.active_environment {
                        println!("Current active context: {}", env.green().bold());
                    } else {
                        println!("No active context set. (Defaulting to local provider)");
                    }
                }
            }
        }
        Commands::Config { command } => {
            match command {
                ConfigCommands::Migrate => {
                    println!("{} Migrating configuration...", "->".blue());
                    match DevpodConfig::migrate(&cli.config) {
                        Ok(_) => {
                            println!("{} Migration complete.", "OK".green());
                        }
                        Err(e) => {
                            println!("{} Migration failed: {}", "!".red(), e);
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        Commands::Doctor { env } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            run_doctor(&config, resolved_env).await?;
        }
        Commands::Repair { env } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            run_repair(&config, &cli.config, resolved_env).await?;
        }
        Commands::Init { .. } => unreachable!(),
    }

    Ok(())
}

async fn run_doctor(config: &DevpodConfig, env_name: Option<&str>) -> Result<()> {
    println!("{} Running Devpod Edge Doctor diagnostics...", "->".blue().bold());
    let mut ok = true;

    // 1. Config version / check
    println!("\n[1/5] Checking configuration files...");
    println!("  * Base config path: {}", "devpod.toml".cyan());
    println!("  * Schema version: {}", config.schema_version);
    println!("  * Total clusters loaded: {}", config.cluster.len());
    for name in config.cluster.keys() {
        println!("    - Found cluster: {}", name.cyan());
    }

    // 2. Local CLI dependencies
    println!("\n[2/5] Checking local CLI dependencies...");
    let deps = vec![
        ("kubectl", vec!["version", "--client"]),
        ("ssh", vec!["-V"]),
        ("tailscale", vec!["version"]),
    ];
    for (dep, args) in deps {
        print!("  * Checking {} ... ", dep);
        std::io::stdout().flush().unwrap();
        match tokio::process::Command::new(dep)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
        {
            Ok(status) if status.success() => {
                println!("{}", "FOUND".green());
            }
            _ => {
                println!("{}", "MISSING or ERROR".red());
                ok = false;
            }
        }
    }

    // 3. Context & active cluster resolve
    println!("\n[3/5] Resolving target environment...");
    if let Some(env) = env_name {
        println!("  * Target environment specified: {}", env.cyan());
        if let Some(cluster) = config.get_cluster(env) {
            println!("    - Provider: {}", cluster.provider);
            println!("    - User: {}", cluster.user.as_deref().unwrap_or("root"));
            println!("    - Nodes count: {}", cluster.nodes.len());

            // 4. Remote Node Reachability / Setup Check
            println!("\n[4/5] Checking node connectivity and setups...");
            let user = cluster.user.as_deref().unwrap_or("root");
            for node in &cluster.nodes {
                let node_name = node.stable_name();
                print!("  * Node {} ({}) ... ", node_name.cyan(), node.role);
                std::io::stdout().flush().unwrap();
                if let Some(target) = resolve_node_host(cluster, node, user).await {
                    match RemoteExecutor::execute(&target, user, "uname -a").await {
                        Ok(_) => {
                            println!("{}", "REACHABLE (SSH OK)".green());
                            
                            // Check cgroups setup on the node
                            print!("    - Checking cgroups status on node ... ");
                            std::io::stdout().flush().unwrap();
                            let check_cgroups = "if grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt || grep -q 'cgroup_memory=1' /boot/cmdline.txt; then echo ok; else echo missing; fi";
                            match RemoteExecutor::execute(&target, user, check_cgroups).await {
                                Ok(out) if out.trim() == "ok" => println!("{}", "CONFIGURED".green()),
                                _ => {
                                    println!("{}", "MISSING cgroups settings (needs setup)".yellow());
                                    ok = false;
                                }
                            }
                        }
                        Err(e) => {
                            println!("{} (SSH error: {})", "UNREACHABLE".red(), e.to_string().trim());
                            ok = false;
                        }
                    }
                } else {
                    println!("{}", "UNCONFIGURED (No address)".red());
                    ok = false;
                }
            }
        } else {
            println!("  * {} Target environment '{}' is not defined in config", "!".red(), env);
            ok = false;
        }
    } else {
        println!("  * Target environment: Local k3d/k3s provider");
        // Check container runtime
        println!("\n[4/5] Checking local container runtime...");
        print!("  * Docker daemon status ... ");
        std::io::stdout().flush().unwrap();
        match tokio::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
        {
            Ok(status) if status.success() => {
                println!("{}", "RUNNING".green());
            }
            _ => {
                println!("{}", "NOT RUNNING (Start Docker daemon)".red());
                ok = false;
            }
        }
    }

    // 5. Tailscale status check
    println!("\n[5/5] Checking Tailscale status...");
    if let Some(env) = env_name {
        if let Some(cluster) = config.get_cluster(env) {
            if cluster.tailscale_enabled() {
                println!("  * Tailscale integration: {}", "ENABLED".green());
                print!("  * Local tailscale status ... ");
                std::io::stdout().flush().unwrap();
                match tokio::process::Command::new("tailscale")
                    .arg("status")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                {
                    Ok(status) if status.success() => {
                        println!("{}", "CONNECTED".green());
                    }
                    _ => {
                        println!("{}", "DISCONNECTED (Run 'tailscale up')".yellow());
                        ok = false;
                    }
                }
            } else {
                println!("  * Tailscale integration: {}", "DISABLED".yellow());
            }
        }
    } else {
        println!("  * Local provider does not require Tailscale.");
    }

    println!("\n=== DIAGNOSTIC SUMMARY ===");
    if ok {
        println!("{} All checks passed! Your environment is healthy.", "SUCCESS".green().bold());
    } else {
        println!("{} Diagnostics found issues. Run 'devpod repair' to automatically fix repairable issues.", "WARNING".yellow().bold());
    }

    Ok(())
}

async fn run_repair(config: &DevpodConfig, config_path: &str, env_name: Option<&str>) -> Result<()> {
    println!("{} Starting Devpod Edge Auto-Repair...", "->".blue().bold());

    // 1. Repair config version if version 0
    if config.schema_version == 0 {
        println!("{} Legacy configuration schema (version 0) detected.", "->".yellow());
        print!("  * Attempting to migrate config ... ");
        std::io::stdout().flush().unwrap();
        match DevpodConfig::migrate(config_path) {
            Ok(_) => println!("{}", "SUCCESS".green()),
            Err(e) => println!("{} ({})", "FAILED".red(), e),
        }
    }

    // Load re-migrated/loaded config if it was migrated
    let config = DevpodConfig::load(config_path).unwrap_or_else(|_| config.clone());

    // 2. Check context validity
    let state = config::DevpodState::load(config_path);
    if let Some(ref env) = state.active_environment {
        if !config.cluster.contains_key(env) {
            println!("{} Active context '{}' points to a missing cluster.", "->".yellow(), env);
            print!("  * Resetting active context to default/local ... ");
            std::io::stdout().flush().unwrap();
            let mut state = state;
            state.active_environment = None;
            let _ = state.save(config_path);
            println!("{}", "SUCCESS".green());
        }
    }

    // 3. Resolve target
    if let Some(env) = env_name {
        if let Some(cluster) = config.get_cluster(env) {
            if cluster.provider == "k3s" {
                println!("{} Remote K3s environment '{}' detected.", "->".blue(), env);
                
                // Let's check node reachability and perform setup/repair
                let user = cluster.user.as_deref().unwrap_or("root");
                for node in &cluster.nodes {
                    let node_name = node.stable_name();
                    println!("  * Checking Node: {} ... ", node_name.cyan());
                    if let Some(target) = resolve_node_host(cluster, node, user).await {
                        // Check if cgroups or deps need repair
                        let check_cgroups = "if grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt || grep -q 'cgroup_memory=1' /boot/cmdline.txt; then echo ok; else echo missing; fi";
                        let cgroups_ok = match RemoteExecutor::execute(&target, user, check_cgroups).await {
                            Ok(out) => out.trim() == "ok",
                            _ => false,
                        };

                        if !cgroups_ok {
                            println!("    - {} Node needs cgroups and dependency setup.", "->".yellow());
                            println!("    - Running setup for this node ... ");
                            let cmd_cgroups = "if ! grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt; then 
                                sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/firmware/cmdline.txt
                                echo 'Updated /boot/firmware/cmdline.txt'
                            elif ! grep -q 'cgroup_memory=1' /boot/cmdline.txt; then
                                sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/cmdline.txt
                                 echo 'Updated /boot/cmdline.txt'
                            else
                                echo 'Cgroups already configured'
                            fi";
                            let running_cgroups = format!("      * Configuring cgroups ... {}", "∨".blue());
                            let success_cgroups = format!("      * Configuring cgroups ... {}", "OK".green());
                            let failure_cgroups = format!("      * Configuring cgroups ... {}", "FAILED".red());
                            let _ = RemoteExecutor::execute_live(
                                &target,
                                user,
                                cmd_cgroups,
                                &running_cgroups,
                                &success_cgroups,
                                &failure_cgroups,
                            )
                            .await;

                            let cmd_deps = "sudo apt-get update && sudo apt-get install -y curl unzip";
                            let running_deps = format!("      * Installing curl and unzip ... {}", "∨".blue());
                            let success_deps = format!("      * Installing curl and unzip ... {}", "OK".green());
                            let failure_deps = format!("      * Installing curl and unzip ... {}", "FAILED".red());
                            let _ = RemoteExecutor::execute_live(
                                &target,
                                user,
                                cmd_deps,
                                &running_deps,
                                &success_deps,
                                &failure_deps,
                            )
                            .await;

                            println!("      * {} Rebooting node to apply cgroups changes ... ", "->".yellow());
                            let _ = RemoteExecutor::execute(&target, user, "sudo reboot").await;
                        } else {
                            println!("    - {}", "Node requirements are healthy".green());
                        }
                    } else {
                        println!("    - {}", "Node is unreachable. Check SSH key, Tailscale connectivity or IP address.".red());
                    }
                }

                // 4. Sync context kubeconfig
                println!("  * Refreshing kubeconfig context ...");
                let manager = RemoteManager::new(env);
                match manager.sync_context(&config).await {
                    Ok(_) => println!("    - {}", "Kubeconfig successfully synchronized".green()),
                    Err(e) => println!("    - {} (sync-context failed: {})", "FAILED".red(), e),
                }
            }
        }
    } else {
        println!("{} Repair running on local context. Checking Docker daemon...", "->".blue());
        match tokio::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
        {
            Ok(status) if status.success() => {
                println!("  * Local container runtime is healthy.");
            }
            _ => {
                println!("  * {} Docker daemon is not running. Please start Docker Desktop/daemon to resolve.", "!".red());
            }
        }
    }

    println!("\n{} Repair process finished.", "OK".green());
    Ok(())
}

