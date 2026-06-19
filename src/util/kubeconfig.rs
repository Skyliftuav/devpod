use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Structures for kubeconfig merging

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterRef {
    pub server: String,
    #[serde(flatten)]
    pub data: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterEntry {
    pub name: String,
    pub cluster: ClusterRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRef {
    pub cluster: String,
    pub user: String,
    #[serde(flatten)]
    pub data: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    pub name: String,
    pub context: ContextRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEntry {
    pub name: String,
    pub user: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Kubeconfig {
    #[serde(rename = "apiVersion", default)]
    pub api_version: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub clusters: Vec<ClusterEntry>,
    #[serde(default)]
    pub contexts: Vec<ContextEntry>,
    #[serde(rename = "current-context", default)]
    pub current_context: String,
    #[serde(default)]
    pub preferences: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub users: Vec<UserEntry>,
}

impl Kubeconfig {
    pub fn has_context(&self, name: &str) -> bool {
        self.contexts.iter().any(|context| context.name == name)
    }

    pub fn existing_contexts(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter(|name| self.has_context(name))
            .cloned()
            .collect()
    }
}

pub fn default_kubeconfig_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("No home dir")?;
    Ok(home.join(".kube").join("config"))
}

pub fn load_kubeconfig(path: impl AsRef<Path>) -> Result<Kubeconfig> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(Kubeconfig::default());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read kubeconfig at {}", path.display()))?;
    serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse kubeconfig at {}", path.display()))
}

pub fn load_default_kubeconfig() -> Result<Kubeconfig> {
    load_kubeconfig(default_kubeconfig_path()?)
}

/// Merge `incoming` kubeconfig into `base` kubeconfig.
/// Updates existing entries by name, or appends new ones.
pub fn merge_kubeconfig(base: &mut Kubeconfig, incoming: Kubeconfig) {
    if base.api_version.is_empty() {
        base.api_version = incoming.api_version.clone();
    }
    if base.kind.is_empty() {
        base.kind = incoming.kind.clone();
    }

    // Merge clusters
    for incoming_cluster in incoming.clusters {
        if let Some(existing) = base
            .clusters
            .iter_mut()
            .find(|c| c.name == incoming_cluster.name)
        {
            *existing = incoming_cluster;
        } else {
            base.clusters.push(incoming_cluster);
        }
    }

    // Merge contexts
    for incoming_context in incoming.contexts {
        if let Some(existing) = base
            .contexts
            .iter_mut()
            .find(|c| c.name == incoming_context.name)
        {
            *existing = incoming_context;
        } else {
            base.contexts.push(incoming_context);
        }
    }

    // Merge users
    for incoming_user in incoming.users {
        if let Some(existing) = base.users.iter_mut().find(|u| u.name == incoming_user.name) {
            *existing = incoming_user;
        } else {
            base.users.push(incoming_user);
        }
    }

    // Usually we update current_context to the incoming one if we are explicitly fetching it.
    if !incoming.current_context.is_empty() {
        base.current_context = incoming.current_context;
    }
}

pub fn remove_entries_by_name(base: &mut Kubeconfig, names: &[String]) {
    base.clusters
        .retain(|cluster| !names.contains(&cluster.name));
    base.contexts
        .retain(|context| !names.contains(&context.name));
    base.users.retain(|user| !names.contains(&user.name));

    if names.contains(&base.current_context) {
        base.current_context.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{load_kubeconfig, remove_entries_by_name, Kubeconfig};

    #[test]
    fn remove_entries_by_name_prunes_only_matching_entries() {
        let mut kubeconfig: Kubeconfig = serde_yaml::from_str(
            r#"
apiVersion: v1
kind: Config
clusters:
  - name: keep
    cluster:
      server: https://keep:6443
  - name: devpod-devlab-tailnet
    cluster:
      server: https://tailnet:6443
contexts:
  - name: keep
    context:
      cluster: keep
      user: keep
  - name: devpod-devlab-tailnet
    context:
      cluster: devpod-devlab-tailnet
      user: devpod-devlab-tailnet
users:
  - name: keep
    user: {}
  - name: devpod-devlab-tailnet
    user: {}
current-context: devpod-devlab-tailnet
"#,
        )
        .unwrap();

        remove_entries_by_name(&mut kubeconfig, &[String::from("devpod-devlab-tailnet")]);

        assert_eq!(kubeconfig.clusters.len(), 1);
        assert_eq!(kubeconfig.clusters[0].name, "keep");
        assert_eq!(kubeconfig.contexts.len(), 1);
        assert_eq!(kubeconfig.contexts[0].name, "keep");
        assert_eq!(kubeconfig.users.len(), 1);
        assert_eq!(kubeconfig.users[0].name, "keep");
        assert!(kubeconfig.current_context.is_empty());
    }

    #[test]
    fn kubeconfig_reports_existing_named_contexts() {
        let kubeconfig: Kubeconfig = serde_yaml::from_str(
            r#"
apiVersion: v1
kind: Config
contexts:
  - name: devpod-edge-direct
    context:
      cluster: devpod-edge-direct
      user: devpod-edge-direct
  - name: unrelated
    context:
      cluster: unrelated
      user: unrelated
"#,
        )
        .unwrap();

        assert!(kubeconfig.has_context("devpod-edge-direct"));
        assert!(!kubeconfig.has_context("devpod-edge-tailnet"));
        assert_eq!(
            kubeconfig.existing_contexts(&[
                "devpod-edge-tailnet".to_string(),
                "devpod-edge-direct".to_string()
            ]),
            vec!["devpod-edge-direct".to_string()]
        );
    }

    #[test]
    fn load_kubeconfig_returns_default_when_file_is_absent() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_path = temp_dir.path().join("missing-config");

        let loaded = load_kubeconfig(&missing_path).unwrap();

        assert!(loaded.contexts.is_empty());
        assert!(loaded.current_context.is_empty());
    }
}
