# Devpod

Devpod is a local edge development orchestrator. It runs Kubernetes-based applications on edge devices (such as ships, vans, or remote servers) using the same workflow as your local machine.

It integrates `sailr` with a local Kubernetes cluster (`k3d` or `k3s`) to provide a unified workflow for building, packaging, and deploying applications in both connected and offline environments.

## Features

-   **Hybrid Orchestrator**: Automatically detects your OS. On **Linux**, it manages a native `k3s` process. On **macOS/Windows**, it provisions a containerized `k3d` cluster.
-   **Simple Commands**: Provides straightforward basic commands. `devpod up` to start and `devpod down` to stop, abstracting away manual cluster management.
-   **Unified Workflow**: Uses the same configuration for local development and production deployment, reducing consistency issues.
-   **Sailr Powered**: Uses `sailr` for builds and templating.

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

4.  **Shut down the environment**:
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

## Contributing

Contributions are welcome. Please open an issue or submit a pull request if you find a bug or have a suggestion to improve the tool.
