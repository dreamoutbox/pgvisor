use std::fs;
use std::path::Path;

use crate::error::CliError;

/// Discovers required healthcheck services from `<dir>/docker-compose.yml`.
pub struct ClusterServices;

impl ClusterServices {
    /// Returns the ordered list of compose services that must report `healthy`.
    pub fn required_for_compose(compose_path: &Path) -> Result<Vec<String>, CliError> {
        let content = fs::read_to_string(compose_path).map_err(|source| CliError::Io {
            path: compose_path.to_path_buf(),
            source,
        })?;

        let mut services = Vec::with_capacity(4);
        if Self::has_service(&content, "rustfs") {
            services.push("rustfs".to_string());
        } else if Self::has_service(&content, "minio") {
            services.push("minio".to_string());
        }

        for node in ["pgvisor-node1", "pgvisor-node2", "pgvisor-node3"] {
            if Self::has_service(&content, node) {
                services.push(node.to_string());
            }
        }

        if services.is_empty() {
            services.extend([
                "pgvisor-node1".to_string(),
                "pgvisor-node2".to_string(),
                "pgvisor-node3".to_string(),
            ]);
        }

        Ok(services)
    }

    fn has_service(compose_yaml: &str, name: &str) -> bool {
        let needle = format!("  {name}:");
        compose_yaml
            .lines()
            .any(|line| line.trim_end() == needle)
    }
}
