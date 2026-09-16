use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use tar::{Archive, Builder, Header};

use super::manager::{BackupError, BasebackupMeta};

/// Generates a standardized snapshot ID, including sanitized label if provided.
pub fn generate_snapshot_id(label: Option<&str>, now: DateTime<Utc>) -> String {
    let timestamp = now.format("%Y%m%d-%H%M%S");
    match label.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(lbl) => {
            let clean: String = lbl
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            format!("{}-snap-{}", clean, timestamp)
        }
        None => format!("snap-{}", timestamp),
    }
}

/// Parsed information from PostgreSQL's backup_label file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedBackupLabel {
    pub start_lsn: Option<String>,
    pub start_wal_file: Option<String>,
    pub checkpoint_location: Option<String>,
    pub backup_method: Option<String>,
    pub backup_from: Option<String>,
    pub start_time: Option<String>,
    pub label: Option<String>,
    pub timeline: Option<u64>,
}

/// Parses PostgreSQL backup_label text into structured metadata.
pub fn parse_backup_label(content: &str) -> ParsedBackupLabel {
    let mut parsed = ParsedBackupLabel::default();
    for line in content.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("START WAL LOCATION:") {
            let val = val.trim();
            // Expected format: "0/3000028 (file 000000010000000000000003)"
            if let Some(paren_start) = val.find("(file ") {
                let lsn = val[..paren_start].trim();
                parsed.start_lsn = Some(lsn.to_string());
                let wal_part = &val[paren_start + 6..];
                if let Some(paren_end) = wal_part.find(')') {
                    parsed.start_wal_file = Some(wal_part[..paren_end].trim().to_string());
                }
            } else {
                parsed.start_lsn = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("CHECKPOINT LOCATION:") {
            parsed.checkpoint_location = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("BACKUP METHOD:") {
            parsed.backup_method = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("BACKUP FROM:") {
            parsed.backup_from = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("START TIME:") {
            parsed.start_time = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("LABEL:") {
            parsed.label = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("START TIMELINE:") {
            if let Ok(tl) = val.trim().parse::<u64>() {
                parsed.timeline = Some(tl);
            }
        }
    }
    parsed
}

/// Processes a PostgreSQL basebackup tar.gz archive:
/// 1. Excludes `backup_label.old` if present.
/// 2. Parses `backup_label` to extract timeline, start WAL, checkpoint location, etc.
/// 3. Parses `PG_VERSION` if present.
/// 4. Injects `metadata.json` at the root of the archive.
/// 5. Writes the filtered and enriched archive to the output writer.
pub fn process_basebackup_archive<R: Read, W: Write>(
    reader: R,
    writer: W,
    meta: &mut BasebackupMeta,
) -> Result<(), BackupError> {
    let gz_decoder = GzDecoder::new(reader);
    let mut archive = Archive::new(gz_decoder);

    let gz_encoder = GzEncoder::new(writer, Compression::default());
    let mut builder = Builder::new(gz_encoder);

    meta.backup_id = Some(meta.snapshot_id.clone());

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let path_str = path.to_string_lossy();

        // 1. Exclude backup_label.old
        if path_str == "backup_label.old" || path_str.ends_with("/backup_label.old") {
            continue;
        }

        // 2. Parse backup_label and pass through
        if path_str == "backup_label" || path_str.ends_with("/backup_label") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            let parsed = parse_backup_label(&content);
            if let Some(tl) = parsed.timeline {
                meta.timeline = Some(tl);
            }
            if let Some(wal) = parsed.start_wal_file {
                meta.start_wal = wal;
            }
            if let Some(lsn) = parsed.start_lsn {
                meta.start_lsn = Some(lsn);
            }
            if let Some(chk) = parsed.checkpoint_location {
                meta.checkpoint_location = Some(chk);
            }
            if let Some(from) = parsed.backup_from {
                meta.backup_from = Some(from);
            }

            let mut header = entry.header().clone();
            builder.append_data(&mut header, &path, content.as_bytes())?;
            continue;
        }

        // 3. Parse PG_VERSION and pass through
        if path_str == "PG_VERSION" || path_str.ends_with("/PG_VERSION") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            meta.pg_version = Some(content.trim().to_string());

            let mut header = entry.header().clone();
            builder.append_data(&mut header, &path, content.as_bytes())?;
            continue;
        }

        // 4. Copy all other entries unchanged
        let mut header = entry.header().clone();
        builder.append_data(&mut header, &path, &mut entry)?;
    }

    // 5. Append metadata.json at root of archive
    let meta_json = serde_json::to_vec_pretty(meta)?;
    let mut meta_header = Header::new_gnu();
    meta_header.set_path("metadata.json")?;
    meta_header.set_size(meta_json.len() as u64);
    meta_header.set_mode(0o644);
    meta_header.set_cksum();
    builder.append_data(&mut meta_header, "metadata.json", meta_json.as_slice())?;

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

/// Creates a minimal valid simulated basebackup tar.gz containing metadata.json and backup_label.
pub fn create_simulated_basebackup(meta: &mut BasebackupMeta) -> Result<Vec<u8>, BackupError> {
    let mut out = Vec::new();
    meta.backup_id = Some(meta.snapshot_id.clone());

    let gz_encoder = GzEncoder::new(&mut out, Compression::default());
    let mut builder = Builder::new(gz_encoder);

    let backup_label_content = format!(
        "START WAL LOCATION: 0/3000028 (file {})\nCHECKPOINT LOCATION: 0/3000080\nBACKUP METHOD: simulated\nBACKUP FROM: {}\nSTART TIME: {}\nLABEL: {}\nSTART TIMELINE: {}\n",
        meta.start_wal,
        meta.source_node.as_deref().unwrap_or("primary"),
        meta.created_at.format("%Y-%m-%d %H:%M:%S GMT"),
        meta.label.as_deref().unwrap_or("simulated backup"),
        meta.timeline.unwrap_or(1),
    );

    let mut label_header = Header::new_gnu();
    label_header.set_path("backup_label")?;
    label_header.set_size(backup_label_content.len() as u64);
    label_header.set_mode(0o644);
    label_header.set_cksum();
    builder.append_data(
        &mut label_header,
        "backup_label",
        backup_label_content.as_bytes(),
    )?;

    let meta_json = serde_json::to_vec_pretty(meta)?;
    let mut meta_header = Header::new_gnu();
    meta_header.set_path("metadata.json")?;
    meta_header.set_size(meta_json.len() as u64);
    meta_header.set_mode(0o644);
    meta_header.set_cksum();
    builder.append_data(&mut meta_header, "metadata.json", meta_json.as_slice())?;

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::manager::BackupType;

    #[test]
    fn test_generate_snapshot_id_formatting() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-16T00:07:11Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            generate_snapshot_id(Some("full1"), now),
            "full1-snap-20260916-000711"
        );
        assert_eq!(
            generate_snapshot_id(Some("incr2"), now),
            "incr2-snap-20260916-000711"
        );
        assert_eq!(
            generate_snapshot_id(Some("weekly backup!"), now),
            "weekly-backup--snap-20260916-000711"
        );
        assert_eq!(
            generate_snapshot_id(Some("   "), now),
            "snap-20260916-000711"
        );
        assert_eq!(generate_snapshot_id(None, now), "snap-20260916-000711");
    }

    #[test]
    fn test_parse_backup_label() {
        let raw = "\
START WAL LOCATION: 0/3000028 (file 000000010000000000000003)
CHECKPOINT LOCATION: 0/3000080
BACKUP METHOD: streamed
BACKUP FROM: standby
START TIME: 2026-09-16 00:07:11 GMT
LABEL: pg_basebackup base backup
START TIMELINE: 1
";
        let parsed = parse_backup_label(raw);
        assert_eq!(parsed.start_lsn.as_deref(), Some("0/3000028"));
        assert_eq!(
            parsed.start_wal_file.as_deref(),
            Some("000000010000000000000003")
        );
        assert_eq!(parsed.checkpoint_location.as_deref(), Some("0/3000080"));
        assert_eq!(parsed.backup_method.as_deref(), Some("streamed"));
        assert_eq!(parsed.backup_from.as_deref(), Some("standby"));
        assert_eq!(
            parsed.start_time.as_deref(),
            Some("2026-09-16 00:07:11 GMT")
        );
        assert_eq!(parsed.timeline, Some(1));
    }

    #[test]
    fn test_process_basebackup_archive_filters_and_injects_metadata() {
        // Create an input tar.gz containing backup_label, backup_label.old, and PG_VERSION
        let mut in_bytes = Vec::new();
        {
            let gz = GzEncoder::new(&mut in_bytes, Compression::default());
            let mut builder = Builder::new(gz);

            let label_content = "START WAL LOCATION: 0/3000028 (file 000000010000000000000003)\nCHECKPOINT LOCATION: 0/3000080\nSTART TIMELINE: 2\n";
            let mut h1 = Header::new_gnu();
            h1.set_path("backup_label").unwrap();
            h1.set_size(label_content.len() as u64);
            h1.set_mode(0o644);
            h1.set_cksum();
            builder
                .append_data(&mut h1, "backup_label", label_content.as_bytes())
                .unwrap();

            let old_content = "obsolete content";
            let mut h2 = Header::new_gnu();
            h2.set_path("backup_label.old").unwrap();
            h2.set_size(old_content.len() as u64);
            h2.set_mode(0o644);
            h2.set_cksum();
            builder
                .append_data(&mut h2, "backup_label.old", old_content.as_bytes())
                .unwrap();

            let version_content = "16\n";
            let mut h3 = Header::new_gnu();
            h3.set_path("PG_VERSION").unwrap();
            h3.set_size(version_content.len() as u64);
            h3.set_mode(0o644);
            h3.set_cksum();
            builder
                .append_data(&mut h3, "PG_VERSION", version_content.as_bytes())
                .unwrap();

            builder.into_inner().unwrap().finish().unwrap();
        }

        let mut meta = BasebackupMeta {
            snapshot_id: "test1-snap-20260916-000711".to_string(),
            backup_id: None,
            created_at: Utc::now(),
            backup_type: BackupType::Full,
            label: Some("test1".to_string()),
            backup_start_date: None,
            backup_finish_date: None,
            timeline: None,
            start_wal: "000000010000000000000001".to_string(),
            stop_wal: None,
            total_bytes: 0,
            source_node: Some("node2".to_string()),
            start_lsn: None,
            checkpoint_location: None,
            backup_from: None,
            pg_version: None,
        };

        let mut out_bytes = Vec::new();
        process_basebackup_archive(in_bytes.as_slice(), &mut out_bytes, &mut meta).unwrap();

        // Verify meta was updated from backup_label and PG_VERSION
        assert_eq!(meta.timeline, Some(2));
        assert_eq!(meta.start_wal, "000000010000000000000003");
        assert_eq!(meta.start_lsn.as_deref(), Some("0/3000028"));
        assert_eq!(meta.checkpoint_location.as_deref(), Some("0/3000080"));
        assert_eq!(meta.pg_version.as_deref(), Some("16"));
        assert_eq!(
            meta.backup_id.as_deref(),
            Some("test1-snap-20260916-000711")
        );

        // Verify out_bytes contents
        let decoder = GzDecoder::new(out_bytes.as_slice());
        let mut archive = Archive::new(decoder);
        let mut found_metadata = false;
        let mut found_old_label = false;
        let mut found_label = false;

        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if path == "metadata.json" {
                found_metadata = true;
                let mut json_str = String::new();
                entry.read_to_string(&mut json_str).unwrap();
                let parsed_meta: serde_json::Value = serde_json::from_str(&json_str).unwrap();
                assert_eq!(parsed_meta["backup_id"], "test1-snap-20260916-000711");
                assert_eq!(parsed_meta["timeline"], 2);
                assert_eq!(parsed_meta["pg_version"], "16");
                assert_eq!(parsed_meta["label"], "test1");
            } else if path == "backup_label.old" {
                found_old_label = true;
            } else if path == "backup_label" {
                found_label = true;
            }
        }

        assert!(found_metadata, "metadata.json was not injected");
        assert!(found_label, "backup_label was not preserved");
        assert!(!found_old_label, "backup_label.old was NOT excluded");
    }
}
