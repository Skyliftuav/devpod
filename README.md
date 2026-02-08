# Devpod

Devpod is your friendly neighborhood edge development orchestrator. It makes running Kubernetes-based applications on edge devices (like ships, vans, or remote servers) just as easy as running them on your laptop.

Think of it as the glue that binds `sailr` and a local Kubernetes cluster (`k3d` or `k3s`) together, giving you a simple, unified workflow for building, packaging, and deploying your apps—whether you have an internet connection or are completely offline.

## Why Devpod?

-   **Hybrid Orchestrator**: Automatically detects your OS. On **Linux**, it manages a native `k3s` process for minimal overhead. On **macOS/Windows**, it spins up a containerized `k3d` cluster.
-   **Simple Commands**: Usage is straightforward. `devpod up` to start, `devpod down` to stop. No more memorizing complex `kubectl` or `docker` incantations.
-   **Unified Workflow**: Use the exact same tools and config for local development and production deployment. Stop debugging "it works on my machine" issues.
-   **Sailr Powered**: We leverage `sailr` for the heavy lifting of building and templating, so you get all the power without the complexity.

## Getting Started

### Prerequisites

You'll need a few things installed depending on your OS:

-   [Docker](https://www.docker.com/) (required for building images and running `k3d`)
-   [kubectl](https://kubernetes.io/docs/tasks/tools/)
-   [`sailr`](https://github.com/Adriftdev/sailr) (required for building and generating manifests)

**For macOS/Windows users:**
-   [k3d](https://k3d.io/)

**For Linux users:**
-   [k3s](https://k3s.io/) (the lightweight Kubernetes binary)

### Installation

Clone the repo and build it:

```bash
cargo install --path .
```

### Quick Start

1.  **Initialize a project**:
    ```bash
    devpod init --name my-edge-app
    ```
    This creates a `devpod.toml` configuration file.

2.  **Spin it up**:
    ```bash
    devpod up
    ```
    This will:
    -   Provision a local cluster (k3d or k3s).
    -   Spin up a local container registry (default port 32000).
    -   Build your services using `sailr`.
    -   Generate and apply Kubernetes manifests.

3.  **Check status**:
    ```bash
    devpod status
    ```

4.  **Shut it down**:
    ```bash
    devpod down
    ```

## Configuration

It's all in `devpod.toml`. Here's the schema:

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

We love contributions! If you find a bug or have a cool idea, open an issue or send a PR. Let's make edge computing fun again.
