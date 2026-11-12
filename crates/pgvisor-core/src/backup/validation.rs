use chrono::{DateTime, Duration, NaiveDateTime, Utc};

use super::manager::{BackupError, BasebackupMeta};

/// Parses a recovery target timestamp string into a UTC `DateTime` using the current time
/// to validate that the target is not in the future.
///
/// Accepts standard RFC 3339 formats, ISO-8601, and normalized space-separated date-time strings
/// (e.g. `2026-09-14 03:00:00`). Single-digit hour, minute, or second components are automatically
/// padded with leading zeros.
///
/// # Arguments
/// * `raw` - The unparsed recovery target timestamp string.
///
/// # Example
/// ```rust
/// use pgvisor_core::backup::validation::parse_target_timestamp;
///
/// let dt = parse_target_timestamp("2026-09-14 03:00:00").unwrap();
/// assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-09-14 03:00:00");
/// ```
pub fn parse_target_timestamp(raw: &str) -> Result<DateTime<Utc>, BackupError> {
    parse_target_timestamp_with_now(raw, Utc::now())
}

/// Parses a recovery target timestamp string into a UTC `DateTime` with an explicit `now` reference.
///
/// # Arguments
/// * `raw` - The unparsed recovery target timestamp string.
/// * `now` - The reference time used to check that the target is not in the future.
pub fn parse_target_timestamp_with_now(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, BackupError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(BackupError::InvalidRecoveryTarget(
            "Recovery target timestamp cannot be empty".to_string(),
        ));
    }

    let parsed = if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        dt.with_timezone(&Utc)
    } else if let Ok(dt) = DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%z") {
        dt.with_timezone(&Utc)
    } else {
        // Normalize single-digit components like "2026-09-16 00:35:0" -> "2026-09-16 00:35:00"
        let normalized = {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() == 2 {
                let date_part = parts[0];
                let time_parts: Vec<&str> = parts[1].split(':').collect();
                if time_parts.len() == 3 {
                    let h = if time_parts[0].len() == 1 {
                        format!("0{}", time_parts[0])
                    } else {
                        time_parts[0].to_string()
                    };
                    let m = if time_parts[1].len() == 1 {
                        format!("0{}", time_parts[1])
                    } else {
                        time_parts[1].to_string()
                    };
                    let s = if time_parts[2].len() == 1 {
                        format!("0{}", time_parts[2])
                    } else {
                        time_parts[2].to_string()
                    };
                    format!("{} {}:{}:{}", date_part, h, m, s)
                } else if time_parts.len() == 2 {
                    let h = if time_parts[0].len() == 1 {
                        format!("0{}", time_parts[0])
                    } else {
                        time_parts[0].to_string()
                    };
                    let m = if time_parts[1].len() == 1 {
                        format!("0{}", time_parts[1])
                    } else {
                        time_parts[1].to_string()
                    };
                    format!("{} {}:{}", date_part, h, m)
                } else {
                    trimmed.to_string()
                }
            } else {
                trimmed.to_string()
            }
        };

        let formats = [
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H:%M",
        ];

        let mut matched = None;
        for fmt in &formats {
            if let Ok(naive) = NaiveDateTime::parse_from_str(&normalized, fmt) {
                matched = Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
                break;
            }
        }

        matched.ok_or_else(|| {
            BackupError::InvalidRecoveryTarget(format!(
                "Invalid timestamp '{}'. Expected format: YYYY-MM-DD HH:MM:SS (UTC) or ISO-8601 (e.g. 2026-09-14 03:00:00)",
                raw
            ))
        })?
    };

    if parsed > now + Duration::seconds(10) {
        return Err(BackupError::InvalidRecoveryTarget(format!(
            "Recovery target timestamp ({}) cannot be in the future. Cluster current time is {}.",
            parsed.format("%Y-%m-%d %H:%M:%S UTC"),
            now.format("%Y-%m-%d %H:%M:%S UTC")
        )));
    }

    Ok(parsed)
}

/// Validates that a requested PITR recovery target timestamp falls within the valid range
/// supported by the snapshot and cluster history.
///
/// Ensures:
/// 1. The target timestamp parses correctly into a valid UTC `DateTime`.
/// 2. The target timestamp is not in the future relative to `now` (with a 10s leeway for clock skew).
/// 3. The target timestamp is not earlier than the basebackup finish date (or start date/created_at fallback).
///
/// # Arguments
/// * `target_raw` - Raw string representation of the target timestamp.
/// * `snapshot` - Basebackup metadata for the selected snapshot.
/// * `_all_backups` - Slice of all available basebackup metadata records.
/// * `now` - Current reference time for future-timestamp detection.
///
/// # Example
/// ```rust
/// use chrono::{Duration, Utc};
/// use pgvisor_core::backup::{BackupType, BasebackupMeta};
/// use pgvisor_core::backup::validation::validate_recovery_target_time;
///
/// let now = Utc::now();
/// let mut snap = BasebackupMeta::new("snap-1", now - Duration::hours(2), BackupType::Full, "0/1", 1024);
/// snap.backup_finish_date = Some(now - Duration::hours(1));
///
/// // Target between finish date and now is valid
/// let valid_target = (now - Duration::minutes(30)).format("%Y-%m-%d %H:%M:%S").to_string();
/// assert!(validate_recovery_target_time(&valid_target, &snap, &[snap.clone()], now).is_ok());
///
/// // Target before finish date is rejected
/// let invalid_target = (now - Duration::hours(3)).format("%Y-%m-%d %H:%M:%S").to_string();
/// assert!(validate_recovery_target_time(&invalid_target, &snap, &[snap.clone()], now).is_err());
/// ```
pub fn validate_recovery_target_time(
    target_raw: &str,
    snapshot: &BasebackupMeta,
    _all_backups: &[BasebackupMeta],
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, BackupError> {
    let parsed = parse_target_timestamp_with_now(target_raw, now)?;

    // Determine the lower bound: basebackup cannot roll backward before its completed snapshot
    let min_finish = snapshot
        .backup_finish_date
        .or(snapshot.backup_start_date)
        .unwrap_or(snapshot.created_at);

    // Allow a small 60s tolerance for clock granularity if snapshot creation date was rounded
    if parsed < min_finish - Duration::seconds(60) {
        return Err(BackupError::InvalidRecoveryTarget(format!(
            "Recovery target timestamp ({}) is earlier than basebackup finish time ({}). PostgreSQL forward recovery cannot roll backward.",
            parsed.format("%Y-%m-%d %H:%M:%S UTC"),
            min_finish.format("%Y-%m-%d %H:%M:%S UTC")
        )));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::BackupType;

    #[test]
    fn test_parse_target_timestamp_formats_and_normalization() {
        // Standard UTC string
        let dt = parse_target_timestamp("2026-09-14 03:00:00").unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-14 03:00:00"
        );

        // Single digit second normalization
        let dt_norm = parse_target_timestamp("2026-09-14 03:00:0").unwrap();
        assert_eq!(
            dt_norm.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-14 03:00:00"
        );

        // RFC 3339
        let dt_rfc = parse_target_timestamp("2026-09-14T03:00:00Z").unwrap();
        assert_eq!(
            dt_rfc.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-14 03:00:00"
        );
    }

    #[test]
    fn test_parse_target_timestamp_malformed_and_empty() {
        assert!(matches!(
            parse_target_timestamp("   "),
            Err(BackupError::InvalidRecoveryTarget(_))
        ));

        assert!(matches!(
            parse_target_timestamp("not-a-timestamp"),
            Err(BackupError::InvalidRecoveryTarget(_))
        ));
    }

    #[test]
    fn test_validate_recovery_target_time_valid() {
        let now = Utc::now();
        let mut snap = BasebackupMeta::new(
            "snap-1",
            now - Duration::hours(2),
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );
        snap.backup_finish_date = Some(now - Duration::hours(1));

        let target = (now - Duration::minutes(30))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let res = validate_recovery_target_time(&target, &snap, &[snap.clone()], now);
        assert!(res.is_ok());
    }

    #[test]
    fn test_validate_recovery_target_time_before_finish_date() {
        let now = Utc::now();
        let mut snap = BasebackupMeta::new(
            "snap-1",
            now - Duration::hours(2),
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );
        snap.backup_finish_date = Some(now - Duration::hours(1));

        let target = (now - Duration::hours(3))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let err = validate_recovery_target_time(&target, &snap, &[snap.clone()], now).unwrap_err();
        match err {
            BackupError::InvalidRecoveryTarget(msg) => {
                assert!(msg.contains("earlier than basebackup finish time"));
                assert!(msg.contains("cannot roll backward"));
            }
            _ => panic!("Expected InvalidRecoveryTarget, got {:?}", err),
        }
    }

    #[test]
    fn test_validate_recovery_target_time_future() {
        let now = Utc::now();
        let snap = BasebackupMeta::new(
            "snap-1",
            now - Duration::hours(1),
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );

        let target = (now + Duration::hours(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let err = validate_recovery_target_time(&target, &snap, &[snap.clone()], now).unwrap_err();
        match err {
            BackupError::InvalidRecoveryTarget(msg) => {
                assert!(msg.contains("cannot be in the future"));
            }
            _ => panic!("Expected InvalidRecoveryTarget, got {:?}", err),
        }
    }

    #[test]
    fn test_validate_recovery_target_time_missing_finish_date_fallback() {
        let now = Utc::now();
        // snapshot without finish_date or start_date fallback to created_at
        let snap = BasebackupMeta::new(
            "snap-1",
            now - Duration::hours(2),
            BackupType::Full,
            "000000010000000000000001",
            1024,
        );

        let target = (now - Duration::hours(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(validate_recovery_target_time(&target, &snap, &[snap.clone()], now).is_ok());

        let too_old = (now - Duration::hours(3))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert!(validate_recovery_target_time(&too_old, &snap, &[snap.clone()], now).is_err());
    }
}
