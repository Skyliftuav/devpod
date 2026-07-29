---
name: devpod-development
description: Provides architectural overviews, codebase navigation, design principles, testing protocols, and dependency information for developing and contributing to devpod.
---

# Devpod Development Skill

This skill guides you on how to develop, extend, and debug the `devpod` orchestrator codebase. Activate this skill whenever you need to add features, debug issues, or understand the architecture of devpod.

## 1. Codebase Navigation & Architecture

`devpod` is a Rust-based tool structured into modular domains:

- **`src/main.rs` (CLI Core & Entrypoint)**:
  Handles command-line parsing via `clap` and routes them to their respective initializers or orchestrators. It also hosts the high-level orchestrator verification (`status`, `up`, `down`, `setup`).

- **`src/config/mod.rs` (Config & State Management)**:
  Defines the schema for `devpod.toml` (de/serialized via `serde` and `toml`). Manages the active context state tracking in `.devpod/state.toml`.

- **`src/initializer/mod.rs` (First Contact Initialization)**:
  Handles remote host configuration before Kubernetes provisioning (`init-nodes`). Configures SSH key copy, generates Visudo entries, and installs basic operating system requirements (`curl`, `unzip`, `ca-certificates`, `avahi-daemon`).

- **`src/orchestrator/` (Kubernetes Cluster Providers)**:
  Implements the `ClusterManager` trait for different providers:
  - `k3d.rs`: Provisions and manages local containerized clusters on macOS and Windows.
  - `k3s.rs`: Downloads and manages local native `k3s` instances as system processes on Linux.
  - `remote.rs` (Remote multi-node orchestrator): Standardizes multi-node K3s server/agent installations. Handles Tailscale login, Avahi-daemon hostname configuration, and local kubeconfig file rewrites to generate portable Tailscale and LAN contexts.

- **`src/executor/mod.rs` (Process Execution)**:
  Executes commands locally or interactively, and runs commands over remote nodes via SSH executors.

- **`src/builder/mod.rs` (Container Builds)**:
  Integrates with packaging/build engines like `sailr`.

- **`src/util/kubeconfig.rs` (Kubeconfig Utilities)**:
  Rewrites and manages local `~/.kube/config` entries safely.

---

## 2. Boot Configurations & Cgroups Repair (Pi vs. Jetson)

One of devpod's critical design tenets is **automated, robust boot-level cgroups configuration** on resource-constrained ARM64 nodes. When `devpod setup` or `devpod up` (via auto-repair) is executed, it checks and configures cgroups across three file interfaces:

1. **Newer Raspberry Pi OS**: `/boot/firmware/cmdline.txt`
2. **Older Raspberry Pi OS**: `/boot/cmdline.txt`
3. **NVIDIA Jetson Nano (L4T / Tegra)**: `/boot/extlinux/extlinux.conf` (appends to the `APPEND` line)

### Cgroups Shell Commands (Implemented in `src/main.rs`)

- **Verification Check**:
  ```bash
  if ( [ -f /boot/firmware/cmdline.txt ] && grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt ) || \
     ( [ -f /boot/cmdline.txt ] && grep -q 'cgroup_memory=1' /boot/cmdline.txt ) || \
     ( [ -f /boot/extlinux/extlinux.conf ] && grep -q 'cgroup_memory=1' /boot/extlinux/extlinux.conf ); \
  then echo ok; else echo missing; fi
  ```

- **Configuration Command**:
  ```bash
  if [ -f /boot/firmware/cmdline.txt ]; then
      if ! grep -q 'cgroup_memory=1' /boot/firmware/cmdline.txt; then
          sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/firmware/cmdline.txt
          echo 'Updated /boot/firmware/cmdline.txt'
      fi
  elif [ -f /boot/cmdline.txt ]; then
      if ! grep -q 'cgroup_memory=1' /boot/cmdline.txt; then
          sudo sed -i 's/$/ cgroup_memory=1 cgroup_enable=memory/' /boot/cmdline.txt
          echo 'Updated /boot/cmdline.txt'
      fi
  elif [ -f /boot/extlinux/extlinux.conf ]; then
      if ! grep -q 'cgroup_memory=1' /boot/extlinux/extlinux.conf; then
          sudo sed -i '/^APPEND/ s/$/ cgroup_enable=cpuset cgroup_memory=1 cgroup_enable=memory/' /boot/extlinux/extlinux.conf
          echo 'Updated /boot/extlinux/extlinux.conf'
      fi
  fi
  ```

---

## 3. Development and Testing Workflows

When making code changes or submitting pull requests:

### Run Code Lints & Compilation checks
```bash
cargo check
cargo clippy
```

### Run Unit Tests
Always run the complete test suite to prevent regressions in kubeconfig rewrites, configuration parsers, and connection orders:
```bash
cargo test
```
The test suite compiles and runs 80 unit tests across library modules and CLI endpoints.
