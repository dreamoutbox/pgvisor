//! Standardized highlight logging for cluster lifecycle events.
//!
//! Emits visually distinct log banners (without leading padding) for:
//! - Node startup (`START NODE`)
//! - Node shutdown (`STOP NODE`)
//! - Physical backup creation (`BACKUP WITH FULL/INCREMENTAL BACKUP "..."`)
//! - Database restore (`RESTORE WITH FULL/INCREMENTAL BACKUP "..." [WITH PITR "..."]`)
//! - Leader failover (`LEADER NODE "node1" IS DOWN. LISTENING TO NEW LEADER NODE "node2"`)

/// Formats a 3-line highlight banner with exactly 20 '=' delimiters and no padding on start of text.
pub fn format_highlight_banner(text: &str) -> String {
    let clean = text.trim();
    format!(
        "========================================\n{}\n========================================",
        clean
    )
}

/// Prints a prominent 3-line banner to stdout without leading padding.
pub fn log_highlight(text: &str) {
    println!("{}", format_highlight_banner(text));
}

/// Formats restore event text with backup type, name/label, and optional PITR timestamp.
pub fn format_restore_highlight(
    backup_type: &str,
    backup_name: &str,
    pitr: Option<&str>,
) -> String {
    let b_type = if backup_type.to_ascii_uppercase().contains("INCR") {
        "INCREMENTAL BACKUP"
    } else {
        "FULL BACKUP"
    };

    match pitr {
        Some(target) if !target.trim().is_empty() => {
            format!(
                "RESTORE WITH {} \"{}\" WITH PITR \"{}\"",
                b_type,
                backup_name,
                target.trim()
            )
        }
        _ => {
            format!("RESTORE WITH {} \"{}\"", b_type, backup_name)
        }
    }
}

/// Formats backup creation event text with backup type and name/label.
pub fn format_backup_highlight(backup_type: &str, backup_name: &str) -> String {
    let b_type = if backup_type.to_ascii_uppercase().contains("INCR") {
        "INCREMENTAL BACKUP"
    } else {
        "FULL BACKUP"
    };
    format!("BACKUP WITH {} \"{}\"", b_type, backup_name)
}

/// Formats new leader failover/repoint notification.
pub fn format_leader_down_highlight(old_leader: &str, new_leader: &str) -> String {
    format!(
        "LEADER NODE \"{}\" IS DOWN. LISTENING TO NEW LEADER NODE \"{}\"",
        old_leader, new_leader
    )
}

/// Formats notification when the local node promotes to leader after previous leader is down.
pub fn format_become_leader_highlight(old_leader: &str, my_node: &str) -> String {
    format!(
        "LEADER NODE \"{}\" IS DOWN. NOW I ({}) BECOME LEADER",
        old_leader, my_node
    )
}

/// Extracts a normalized node identifier (e.g. "node1", "node2") from connection strings,
/// host:port strings, or numeric IDs.
pub fn extract_node_name(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return "unknown".to_string();
    }

    // Check for host= in conninfo
    let raw_host = if let Some(idx) = s.find("host=") {
        let after = &s[idx + 5..];
        after.split_whitespace().next().unwrap_or(after)
    } else {
        s.split(':').next().unwrap_or(s)
    };

    let clean = raw_host.trim().trim_matches('\'').trim_matches('"');
    if let Some(stripped) = clean.strip_prefix("pgvisor-") {
        stripped.to_string()
    } else if clean.chars().all(|c| c.is_ascii_digit()) {
        format!("node{}", clean)
    } else {
        clean.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_node_name() {
        assert_eq!(
            extract_node_name("host=pgvisor-node1 port=5432 user=postgres"),
            "node1"
        );
        assert_eq!(extract_node_name("host=pgvisor-node2 port=5432"), "node2");
        assert_eq!(extract_node_name("pgvisor-node1:5432"), "node1");
        assert_eq!(extract_node_name("pgvisor-node3"), "node3");
        assert_eq!(extract_node_name("node2"), "node2");
        assert_eq!(extract_node_name("1"), "node1");
        assert_eq!(extract_node_name("2"), "node2");
    }

    #[test]
    fn test_format_restore_highlight() {
        let full_restore = format_restore_highlight("full", "full1", None);
        assert_eq!(full_restore, "RESTORE WITH FULL BACKUP \"full1\"");

        let incr_pitr =
            format_restore_highlight("incremental", "incr2", Some("026-09-13 18:32:49"));
        assert_eq!(
            incr_pitr,
            "RESTORE WITH INCREMENTAL BACKUP \"incr2\" WITH PITR \"026-09-13 18:32:49\""
        );
    }

    #[test]
    fn test_format_backup_highlight() {
        let full_backup = format_backup_highlight("full", "full1");
        assert_eq!(full_backup, "BACKUP WITH FULL BACKUP \"full1\"");

        let incr_backup = format_backup_highlight("incremental", "incr2");
        assert_eq!(incr_backup, "BACKUP WITH INCREMENTAL BACKUP \"incr2\"");
    }

    #[test]
    fn test_format_leader_down_highlight() {
        let leader_down = format_leader_down_highlight("node1", "node2");
        assert_eq!(
            leader_down,
            "LEADER NODE \"node1\" IS DOWN. LISTENING TO NEW LEADER NODE \"node2\""
        );
    }

    #[test]
    fn test_format_become_leader_highlight() {
        let become_leader = format_become_leader_highlight("node1", "node2");
        assert_eq!(
            become_leader,
            "LEADER NODE \"node1\" IS DOWN. NOW I (node2) BECOME LEADER"
        );
    }

    #[test]
    fn test_banner_delimiter_and_no_padding() {
        let banner = format_highlight_banner("START NODE");
        let lines: Vec<&str> = banner.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "========================================");
        assert_eq!(lines[1], "START NODE");
        assert_eq!(lines[2], "========================================");
        // Verify no leading whitespace
        assert!(!lines[1].starts_with(' '));
        assert!(!lines[1].starts_with('\t'));
    }
}
