use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    base.clusters.retain(|cluster| !names.contains(&cluster.name));
    base.contexts.retain(|context| !names.contains(&context.name));
    base.users.retain(|user| !names.contains(&user.name));

    if names.contains(&base.current_context) {
        base.current_context.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{remove_entries_by_name, Kubeconfig};

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
}
