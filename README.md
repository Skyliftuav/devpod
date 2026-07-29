# Devpod

Devpod is a local edge development orchestrator. It runs Kubernetes-based applications on edge devices (such as ships, vans, or remote servers) using the same workflow as your local machine.

It integrates `sailr` with a local Kubernetes cluster (`k3d` or `k3s`) to provide a unified workflow for building, packaging, and deploying applications in both connected and offline environments.

## Features

-   **Hybrid Orchestrator**: Automatically detects your OS. On **Linux**, it manages a native `k3s` process. On **macOS/Windows**, it provisions a containerized `k3d` cluster.
-   **Simple Commands**: Provides straightforward basic commands. `devpod up` to start and `devpod down` to stop, abstracting away manual cluster management.
-   **Unified Workflow**: Uses the same configuration for local development and production deployment, reducing consistency issues.
-   **Sailr Powered**: Uses `sailr` for builds and templating.
-   **Portable Remote Access**: Remote `k3s` environments can publish stable LAN and Tailscale management endpoints so the same cluster can move between networks without rewriting kubeconfig.

## Getting Started

### Prerequisites

Depending on your OS, install the following:

-   [Docker](https://www.docker.com/) (required for building images and running `k3d`)
-   [kubectl](https://kubernetes.io/docs/tasks/tools/)
-   [`sailr`](https://github.com/Adriftdev/sailr) (required for building and generating manifests)

**For macOS/Windows users:**
-   [k3d](https://k3d.io/)

**For Linux users:**
-   [k3s](https://k3s.io/) (lightweight Kubernetes binary)

### Installation

Clone the repository and install it using Cargo:

```bash
cargo install --path .
```

### Quick Start

1.  **Initialize a project**:
    ```bash
    devpod init --name my-edge-app
    ```
    This creates a `devpod.toml` configuration file.

2.  **Start the environment**:
    ```bash
    devpod up
    ```
    This command will:
    -   Provision a local cluster (k3d or k3s).
    -   Start a local container registry (default port 32000).
    -   Build your services using `sailr`.
    -   Generate and apply Kubernetes manifests.

3.  **Check status**:
    ```bash
    devpod status
    ```

4.  **Select a configured environment**:
    ```bash
    devpod context list
    devpod context use production-van
    devpod context show
    ```
    Devpod stores this active environment in `.devpod/state.toml`. This does not change kubectl's global `current-context`.

    To make plain `kubectl` follow the selected environment too, opt into the global side effect:
    ```bash
    devpod context use production-van --global
    ```
    This runs `kubectl config use-context <resolved-devpod-context>` after validating that the managed kube context exists.

5.  **Initialize remote nodes for first contact**:
    ```bash
    devpod init-nodes --env production-van
    ```
    This copies your SSH key with `ssh-copy-id`, configures passwordless sudo for the configured cluster user, and installs safe baseline prerequisites. Use `--node <ref>` to initialize selected nodes and `--identity <path>` to choose a specific public key.

6.  **Refresh kubeconfig after reinitializing a remote cluster**:
    ```bash
    devpod sync-context --env production-van
    ```
    This refreshes the local `devpod-<env>-tailnet`, `devpod-<env>-lan`, and `devpod-<env>-direct` contexts from the live remote `k3s` server without reprovisioning or redeploying.

7.  **Shut down the environment**:
    ```bash
    devpod down
    ```

## Configuration

Configuration is managed in `devpod.toml` based on the following schema:

```toml
[project]
name = "my-edge-project"

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
# Ports to expose from the cluster (Host -> Container)
expose = [
  { host = 1883, container = 1883, protocol = "TCP" },
  { host = 8080, container = 80, protocol = "HTTP" }
]
```

### Portable Remote K3s Example

```toml
[project]
name = "portable-edge"

[cluster.production-van]
provider = "k3s"
connection = "ssh"
user = "admin"

[[cluster.production-van.nodes]]
role = "server"
name = "control-1"
bootstrap_address = "192.168.1.10"
runtime = "containerd"

[[cluster.production-van.nodes]]
role = "agent"
name = "worker-1"
bootstrap_address = "192.168.1.11"
runtime = "containerd"

[cluster.production-van.access]
mode = "dual"
primary = "tailscale"
lan_domain = "local"
published_ports = [
  { node = "control-1", port = 6443, protocol = "TCP", name = "k8s-api" },
  { node = "worker-1", port = 8080, protocol = "HTTP", name = "dashboard" }
]

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
```

Remote `k3s` provisioning now does the following:

-   `devpod init-nodes --env <env>` handles first-contact SSH key copy, dedicated passwordless sudo setup, and safe baseline packages before provisioning.
-   Uses `bootstrap_address` for first contact and cluster joins on the local network.
-   Sets a stable node hostname from `nodes[].name`, enables Avahi, and generates a LAN kubeconfig context such as `devpod-production-van-lan`.
-   Installs and configures Tailscale when enabled, then generates a Tailscale kubeconfig context such as `devpod-production-van-tailnet`.
-   Also generates a direct-address kubeconfig context such as `devpod-production-van-direct` so on-site management can still work even if MagicDNS or `.local` name resolution is unavailable.
-   Makes the preferred context the primary access mode, usually Tailscale.

When a remote environment is active, `devpod status` runs kubectl with the resolved managed context explicitly, for example `kubectl --context devpod-production-van-tailnet get nodes`. If that kube context is missing, run `devpod sync-context --env production-van`. Devpod will not silently fall back to kubectl's global `current-context`, because that can show the wrong cluster after switching environments.

Set `TAILSCALE_AUTH_KEY` in your shell before running `devpod up` for a portable Tailscale-managed cluster.
Set `TAILSCALE_API_KEY` as well if you want `devpod down` to delete the device from the Tailscale admin plane instead of only logging it out and purging the node locally.

## Contributing

Contributions are welcome. Please open an issue or submit a pull request if you find a bug or have a suggestion to improve the tool.
