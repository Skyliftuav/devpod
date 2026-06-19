use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::io::Write;
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
use orchestrator::remote::RemoteManager;
use util::kubeconfig::Kubeconfig;

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusTarget {
    Local {
        label: String,
        context_name: Option<String>,
    },
    Remote {
        env_name: String,
        context_name: String,
    },
}

impl StatusTarget {
    fn label(&self) -> &str {
        match self {
            StatusTarget::Local { label, .. } => label,
            StatusTarget::Remote { env_name, .. } => env_name,
        }
    }

    fn kubectl_context(&self) -> Option<&str> {
        match self {
            StatusTarget::Local { context_name, .. } => context_name.as_deref(),
            StatusTarget::Remote { context_name, .. } => Some(context_name.as_str()),
        }
    }

    fn display_name(&self) -> String {
        match self {
            StatusTarget::Local {
                label,
                context_name: Some(context_name),
            } => format!("{} ({})", label, context_name),
            StatusTarget::Local { label, .. } => label.clone(),
            StatusTarget::Remote {
                env_name,
                context_name,
            } => format!("{} ({})", env_name, context_name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextUseTarget {
    env_name: String,
    provider: String,
    kubectl_context: Option<String>,
}

fn k3d_context_name(config: &DevpodConfig) -> String {
    format!("k3d-{}", config.project.name)
}

fn resolve_status_target(
    config: &DevpodConfig,
    requested_env: Option<&str>,
    active_env: Option<&str>,
    kubeconfig: &Kubeconfig,
) -> Result<StatusTarget> {
    let Some(env_name) = requested_env.or(active_env) else {
        return Ok(StatusTarget::Local {
            label: "local".to_string(),
            context_name: None,
        });
    };

    let cluster = config.get_cluster(env_name).with_context(|| {
        format!(
            "Environment '{}' not found in config. Run 'devpod context list' to see available contexts.",
            env_name
        )
    })?;

    match cluster.provider.as_str() {
        "k3s" => {
            let manager = RemoteManager::new(env_name);
            let context_name = manager.resolved_kube_context_name(cluster)?;

            if !kubeconfig.has_context(&context_name) {
                let existing = kubeconfig.existing_contexts(&manager.managed_context_names());
                let existing_detail = if existing.is_empty() {
                    "No managed kube contexts are present.".to_string()
                } else {
                    format!("Existing managed contexts: {}.", existing.join(", "))
                };

                anyhow::bail!(
                    "Kube context '{}' for environment '{}' is not configured. {} Run `devpod sync-context --env {}` to refresh it.",
                    context_name,
                    env_name,
                    existing_detail,
                    env_name
                );
            }

            Ok(StatusTarget::Remote {
                env_name: env_name.to_string(),
                context_name,
            })
        }
        "k3d" => {
            let context_name = k3d_context_name(config);
            if !kubeconfig.has_context(&context_name) {
                anyhow::bail!(
                    "Kube context '{}' for local k3d environment '{}' is not configured. Run `devpod up --env {}` to create or refresh it.",
                    context_name,
                    env_name,
                    env_name
                );
            }

            Ok(StatusTarget::Local {
                label: env_name.to_string(),
                context_name: Some(context_name),
            })
        }
        _ => Ok(StatusTarget::Local {
            label: env_name.to_string(),
            context_name: None,
        }),
    }
}

async fn run_kubectl_status(target: &StatusTarget) -> Result<()> {
    let mut command = tokio::process::Command::new("kubectl");

    if let Some(context_name) = target.kubectl_context() {
        command.arg("--context").arg(context_name);
    }

    let status = command
        .args(["get", "nodes"])
        .status()
        .await
        .with_context(|| format!("Failed to run kubectl for {}", target.label()))?;

    if !status.success() {
        anyhow::bail!("kubectl get nodes failed for {}", target.display_name());
    }

    Ok(())
}

fn context_kube_status(
    config: &DevpodConfig,
    env_name: &str,
    cluster: &ClusterDefinition,
    kubeconfig: &Kubeconfig,
) -> String {
    match cluster.provider.as_str() {
        "k3s" => {
            let manager = RemoteManager::new(env_name);
            let managed_contexts = manager.managed_context_names();
            let existing = kubeconfig.existing_contexts(&managed_contexts);

            match manager.preferred_kube_context_name(cluster) {
                Ok(expected) if kubeconfig.has_context(&expected) => {
                    format!("kube: ready ({})", expected)
                }
                Ok(expected) if existing.is_empty() => {
                    format!(
                        "kube: missing expected {} (run `devpod sync-context --env {}`)",
                        expected, env_name
                    )
                }
                Ok(expected) => {
                    format!(
                        "kube: partial, expected {} (present: {})",
                        expected,
                        existing.join(", ")
                    )
                }
                Err(error) => format!("kube: invalid ({})", error),
            }
        }
        "k3d" => {
            let context_name = k3d_context_name(config);
            if kubeconfig.has_context(&context_name) {
                format!("kube: ready ({})", context_name)
            } else {
                format!(
                    "kube: missing expected {} (run `devpod up --env {}`)",
                    context_name, env_name
                )
            }
        }
        _ => "kube: unmanaged".to_string(),
    }
}

fn load_kubeconfig_for_context_output() -> Kubeconfig {
    match util::kubeconfig::load_default_kubeconfig() {
        Ok(kubeconfig) => kubeconfig,
        Err(error) => {
            println!("{} Could not inspect kubeconfig: {}", "!".yellow(), error);
            Kubeconfig::default()
        }
    }
}

fn resolve_context_use_target(
    config: &DevpodConfig,
    env_name: &str,
    global: bool,
    kubeconfig: &Kubeconfig,
) -> Result<ContextUseTarget> {
    let cluster = config.get_cluster(env_name).with_context(|| {
        format!(
            "Environment '{}' not found in config. Run 'devpod context list' to see available contexts.",
            env_name
        )
    })?;

    if !global {
        return Ok(ContextUseTarget {
            env_name: env_name.to_string(),
            provider: cluster.provider.clone(),
            kubectl_context: None,
        });
    }

    let status_target = resolve_status_target(config, Some(env_name), None, kubeconfig)?;
    let Some(context_name) = status_target.kubectl_context() else {
        anyhow::bail!(
            "Environment '{}' uses provider '{}', which does not have a managed kubectl context.",
            env_name,
            cluster.provider
        );
    };

    Ok(ContextUseTarget {
        env_name: env_name.to_string(),
        provider: cluster.provider.clone(),
        kubectl_context: Some(context_name.to_string()),
    })
}

fn kubectl_use_context_args(context_name: &str) -> Vec<String> {
    vec![
        "config".to_string(),
        "use-context".to_string(),
        context_name.to_string(),
    ]
}

async fn set_kubectl_current_context(env_name: &str, context_name: &str) -> Result<()> {
    let args = kubectl_use_context_args(context_name);
    let status = tokio::process::Command::new("kubectl")
        .args(&args)
        .status()
        .await
        .with_context(|| {
            format!(
                "Devpod context was saved as '{}', but kubectl global context was not changed to '{}'",
                env_name, context_name
            )
        })?;

    if !status.success() {
        anyhow::bail!(
            "Devpod context was saved as '{}', but kubectl global context was not changed to '{}'",
            env_name,
            context_name
        );
    }

    Ok(())
}

#[derive(Subcommand)]
enum ContextCommands {
    /// List all available contexts
    List,
    /// Set the active context
    Use {
        /// Context/environment name
        env: String,

        /// Also set kubectl's global current-context
        #[arg(long)]
        global: bool,
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
        Commands::Down {
            env,
            purge_tailscale,
        } => {
            let resolved_env = env.as_deref().or(state.active_environment.as_deref());
            let manager = get_manager(&config, resolved_env);
            manager.down(&config, purge_tailscale).await?;
            println!("{} Environment stopped.", "OK".green());
        }
        Commands::SyncContext { env } => {
            let env_name = resolve_env(env)?;
            let cluster = config
                .get_cluster(&env_name)
                .with_context(|| format!("Environment '{}' not found in config", env_name))?;

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
            let kubeconfig = util::kubeconfig::load_default_kubeconfig()?;
            let target = resolve_status_target(
                &config,
                env.as_deref(),
                state.active_environment.as_deref(),
                &kubeconfig,
            )?;

            println!(
                "{} Checking status for {}...",
                "->".blue(),
                target.display_name()
            );

            run_kubectl_status(&target).await?;
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
                println!(
                    "{} Environment '{}' not found in config",
                    "!".red(),
                    env_name
                );
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
                println!(
                    "{} Setting up nodes for '{}'...",
                    "->".blue().bold(),
                    env_name
                );

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
                    let success_cgroups =
                        format!("      * Configuring cgroups ... {}", "OK".green());
                    let failure_cgroups =
                        format!("      * Configuring cgroups ... {}", "FAILED".red());
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
                    let running_deps =
                        format!("      * Installing dependencies ... {}", "∨".blue());
                    let success_deps =
                        format!("      * Installing dependencies ... {}", "OK".green());
                    let failure_deps =
                        format!("      * Installing dependencies ... {}", "FAILED".red());
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
                println!(
                    "{} Environment '{}' not found in config",
                    "!".red(),
                    env_name
                );
            }
        }
        Commands::Context { command } => match command {
            ContextCommands::List => {
                let kubeconfig = load_kubeconfig_for_context_output();
                let mut cluster_names = config.cluster.keys().collect::<Vec<_>>();
                cluster_names.sort();

                println!("Available contexts:");
                for cluster_name in cluster_names {
                    let marker = if state.active_environment.as_ref() == Some(cluster_name) {
                        "* ".green().bold()
                    } else {
                        "  ".normal()
                    };
                    if let Some(cluster) = config.get_cluster(cluster_name) {
                        println!(
                            "{}{} ({}) - {}",
                            marker,
                            cluster_name,
                            cluster.provider,
                            context_kube_status(&config, cluster_name, cluster, &kubeconfig)
                        );
                    }
                }
            }
            ContextCommands::Use { env, global } => {
                let strict_kubeconfig = if global {
                    Some(util::kubeconfig::load_default_kubeconfig()?)
                } else {
                    None
                };
                let target = resolve_context_use_target(
                    &config,
                    &env,
                    global,
                    strict_kubeconfig.as_ref().unwrap_or(&Kubeconfig::default()),
                )?;

                let mut state = state;
                state.active_environment = Some(target.env_name.clone());
                state.save(&cli.config)?;
                println!("{} Switched to context '{}'", "OK".green(), target.env_name);
                println!("   Provider: {}", target.provider.cyan());

                if let Some(context_name) = target.kubectl_context.as_deref() {
                    set_kubectl_current_context(&target.env_name, context_name).await?;
                    println!(
                        "   {} kubectl current-context set to '{}'",
                        "OK".green(),
                        context_name
                    );
                }

                if let Some(cluster) = config.get_cluster(&target.env_name) {
                    let kubeconfig =
                        strict_kubeconfig.unwrap_or_else(load_kubeconfig_for_context_output);
                    println!(
                        "   {}",
                        context_kube_status(&config, &target.env_name, cluster, &kubeconfig)
                    );
                }
            }
            ContextCommands::Show => {
                if let Some(ref env) = state.active_environment {
                    println!("Current active context: {}", env.green().bold());
                    if let Some(cluster) = config.get_cluster(env) {
                        let kubeconfig = load_kubeconfig_for_context_output();
                        println!("Provider: {}", cluster.provider.cyan());
                        println!(
                            "{}",
                            context_kube_status(&config, env, cluster, &kubeconfig)
                        );
                    } else {
                        println!(
                            "{} Active context '{}' is not defined in config. Run `devpod repair` to reset it.",
                            "!".yellow(),
                            env
                        );
                    }
                } else {
                    println!("No active context set. (Defaulting to local provider)");
                }
            }
        },
        Commands::Config { command } => match command {
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
        },
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
    println!(
        "{} Running Devpod Edge Doctor diagnostics...",
        "->".blue().bold()
    );
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
                                Ok(out) if out.trim() == "ok" => {
                                    println!("{}", "CONFIGURED".green())
                                }
                                _ => {
                                    println!(
                                        "{}",
                                        "MISSING cgroups settings (needs setup)".yellow()
                                    );
                                    ok = false;
                                }
                            }
                        }
                        Err(e) => {
                            println!(
                                "{} (SSH error: {})",
                                "UNREACHABLE".red(),
                                e.to_string().trim()
                            );
                            ok = false;
                        }
                    }
                } else {
                    println!("{}", "UNCONFIGURED (No address)".red());
                    ok = false;
                }
            }
        } else {
            println!(
                "  * {} Target environment '{}' is not defined in config",
                "!".red(),
                env
            );
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
        println!(
            "{} All checks passed! Your environment is healthy.",
            "SUCCESS".green().bold()
        );
    } else {
        println!("{} Diagnostics found issues. Run 'devpod repair' to automatically fix repairable issues.", "WARNING".yellow().bold());
    }

    Ok(())
}

async fn run_repair(
    config: &DevpodConfig,
    config_path: &str,
    env_name: Option<&str>,
) -> Result<()> {
    println!("{} Starting Devpod Edge Auto-Repair...", "->".blue().bold());

    // 1. Repair config version if version 0
    if config.schema_version == 0 {
        println!(
            "{} Legacy configuration schema (version 0) detected.",
            "->".yellow()
        );
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
            println!(
                "{} Active context '{}' points to a missing cluster.",
                "->".yellow(),
                env
            );
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
                        let cgroups_ok =
                            match RemoteExecutor::execute(&target, user, check_cgroups).await {
                                Ok(out) => out.trim() == "ok",
                                _ => false,
                            };

                        if !cgroups_ok {
                            println!(
                                "    - {} Node needs cgroups and dependency setup.",
                                "->".yellow()
                            );
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
                            let running_cgroups =
                                format!("      * Configuring cgroups ... {}", "∨".blue());
                            let success_cgroups =
                                format!("      * Configuring cgroups ... {}", "OK".green());
                            let failure_cgroups =
                                format!("      * Configuring cgroups ... {}", "FAILED".red());
                            let _ = RemoteExecutor::execute_live(
                                &target,
                                user,
                                cmd_cgroups,
                                &running_cgroups,
                                &success_cgroups,
                                &failure_cgroups,
                            )
                            .await;

                            let cmd_deps =
                                "sudo apt-get update && sudo apt-get install -y curl unzip";
                            let running_deps =
                                format!("      * Installing curl and unzip ... {}", "∨".blue());
                            let success_deps =
                                format!("      * Installing curl and unzip ... {}", "OK".green());
                            let failure_deps =
                                format!("      * Installing curl and unzip ... {}", "FAILED".red());
                            let _ = RemoteExecutor::execute_live(
                                &target,
                                user,
                                cmd_deps,
                                &running_deps,
                                &success_deps,
                                &failure_deps,
                            )
                            .await;

                            println!(
                                "      * {} Rebooting node to apply cgroups changes ... ",
                                "->".yellow()
                            );
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
        println!(
            "{} Repair running on local context. Checking Docker daemon...",
            "->".blue()
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ClusterAccessConfig, DeploymentConfig, InfrastructureConfig, NetworkConfig, ProjectConfig,
        RegistryConfig, SecretsConfig, TailscaleConfig,
    };
    use crate::util::kubeconfig::{ContextEntry, ContextRef};
    use std::collections::HashMap;

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
            user: Some("root".to_string()),
            nodes: vec![RemoteNodeConfig {
                role: "server".to_string(),
                name: Some("control-1".to_string()),
                bootstrap_address: Some("127.0.0.1".to_string()),
                address: None,
                runtime: "containerd".to_string(),
                labels: HashMap::new(),
            }],
            datastore_endpoint: None,
            access: ClusterAccessConfig {
                mode: "tailscale-only".to_string(),
                primary: "tailscale".to_string(),
                lan_domain: "local".to_string(),
                published_ports: Vec::new(),
            },
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

    fn kubeconfig_with_context(name: &str) -> Kubeconfig {
        Kubeconfig {
            contexts: vec![ContextEntry {
                name: name.to_string(),
                context: ContextRef {
                    cluster: name.to_string(),
                    user: name.to_string(),
                    data: HashMap::new(),
                },
            }],
            ..Kubeconfig::default()
        }
    }

    #[test]
    fn status_resolves_active_remote_env_to_managed_context() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = kubeconfig_with_context("devpod-edge-direct");

        let target = resolve_status_target(&config, None, Some("edge"), &kubeconfig).unwrap();

        assert_eq!(
            target,
            StatusTarget::Remote {
                env_name: "edge".to_string(),
                context_name: "devpod-edge-direct".to_string()
            }
        );
    }

    #[test]
    fn status_reports_sync_guidance_when_remote_kube_context_is_missing() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = Kubeconfig::default();

        let error = resolve_status_target(&config, None, Some("edge"), &kubeconfig)
            .unwrap_err()
            .to_string();

        assert!(error.contains("devpod-edge-direct"));
        assert!(error.contains("devpod sync-context --env edge"));
    }

    #[test]
    fn status_explicit_env_overrides_active_env() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        clusters.insert("other".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = kubeconfig_with_context("devpod-other-direct");

        let target =
            resolve_status_target(&config, Some("other"), Some("edge"), &kubeconfig).unwrap();

        assert_eq!(
            target,
            StatusTarget::Remote {
                env_name: "other".to_string(),
                context_name: "devpod-other-direct".to_string()
            }
        );
    }

    #[test]
    fn status_unknown_env_fails_with_context_list_guidance() {
        let config = test_config(HashMap::new());
        let kubeconfig = Kubeconfig::default();

        let error = resolve_status_target(&config, Some("missing"), None, &kubeconfig)
            .unwrap_err()
            .to_string();

        assert!(error.contains("Environment 'missing' not found"));
        assert!(error.contains("devpod context list"));
    }

    #[test]
    fn context_use_default_does_not_require_kube_context() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = Kubeconfig::default();

        let target = resolve_context_use_target(&config, "edge", false, &kubeconfig).unwrap();

        assert_eq!(
            target,
            ContextUseTarget {
                env_name: "edge".to_string(),
                provider: "k3s".to_string(),
                kubectl_context: None
            }
        );
    }

    #[test]
    fn context_use_global_resolves_remote_env_to_managed_context() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = kubeconfig_with_context("devpod-edge-direct");

        let target = resolve_context_use_target(&config, "edge", true, &kubeconfig).unwrap();

        assert_eq!(
            target,
            ContextUseTarget {
                env_name: "edge".to_string(),
                provider: "k3s".to_string(),
                kubectl_context: Some("devpod-edge-direct".to_string())
            }
        );
    }

    #[test]
    fn context_use_global_fails_with_sync_guidance_when_context_is_missing() {
        let mut clusters = HashMap::new();
        clusters.insert("edge".to_string(), remote_cluster());
        let config = test_config(clusters);
        let kubeconfig = Kubeconfig::default();

        let error = resolve_context_use_target(&config, "edge", true, &kubeconfig)
            .unwrap_err()
            .to_string();

        assert!(error.contains("devpod-edge-direct"));
        assert!(error.contains("devpod sync-context --env edge"));
    }

    #[test]
    fn context_use_unknown_env_fails_with_context_list_guidance() {
        let config = test_config(HashMap::new());
        let kubeconfig = Kubeconfig::default();

        let error = resolve_context_use_target(&config, "missing", false, &kubeconfig)
            .unwrap_err()
            .to_string();

        assert!(error.contains("Environment 'missing' not found"));
        assert!(error.contains("devpod context list"));
    }

    #[test]
    fn context_use_global_resolves_k3d_env_to_project_context() {
        let mut clusters = HashMap::new();
        clusters.insert("dev".to_string(), k3d_cluster());
        let config = test_config(clusters);
        let kubeconfig = kubeconfig_with_context("k3d-edge-app");

        let target = resolve_context_use_target(&config, "dev", true, &kubeconfig).unwrap();

        assert_eq!(
            target,
            ContextUseTarget {
                env_name: "dev".to_string(),
                provider: "k3d".to_string(),
                kubectl_context: Some("k3d-edge-app".to_string())
            }
        );
    }

    #[test]
    fn kubectl_use_context_args_builds_expected_command_args() {
        assert_eq!(
            kubectl_use_context_args("devpod-edge-direct"),
            vec![
                "config".to_string(),
                "use-context".to_string(),
                "devpod-edge-direct".to_string()
            ]
        );
    }
}
