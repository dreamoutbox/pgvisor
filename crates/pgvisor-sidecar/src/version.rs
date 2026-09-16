use std::path::Path;

/// Detects the installed or running PostgreSQL server version.
pub async fn detect_postgres_version(data_dir: &Path) -> String {
    if let Ok(output) = tokio::process::Command::new("postgres")
        .arg("-V")
        .output()
        .await
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for part in stdout.split_whitespace() {
                let trimmed = part.trim_matches(|c: char| !c.is_ascii_digit() && c != '.');
                if trimmed.contains('.') && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.')
                {
                    return trimmed.to_string();
                }
            }
        }
    }
    let pg_version_file = data_dir.join("PG_VERSION");
    if let Ok(content) = tokio::fs::read_to_string(pg_version_file).await {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    "18.6".to_string()
}
