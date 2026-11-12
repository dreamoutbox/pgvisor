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

    let unadorned = trimmed
        .strip_suffix(" (UTC)")
        .or_else(|| trimmed.strip_suffix(" (utc)"))
        .or_else(|| trimmed.strip_suffix(" UTC"))
        .or_else(|| trimmed.strip_suffix(" utc"))
        .unwrap_or(trimmed)
        .trim();

    let parsed = if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        dt.with_timezone(&Utc)
    } else if let Ok(dt) = DateTime::parse_from_rfc3339(unadorned) {
        dt.with_timezone(&Utc)
    } else if let Ok(dt) = DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%z") {
        dt.with_timezone(&Utc)
    } else if let Ok(dt) = DateTime::parse_from_str(unadorned, "%Y-%m-%d %H:%M:%S%z") {
        dt.with_timezone(&Utc)
    } else {
        // Normalize single-digit components like "2026-09-16 00:35:0" -> "2026-09-16 00:35:00"
        let normalized = {
            let parts: Vec<&str> = unadorned.split_whitespace().collect();
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
                    unadorned.to_string()
                }
            } else {
                unadorned.to_string()
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

/// Validates and parses a recovery target transaction ID (XID).
///
/// Ensures:
/// 1. The XID is non-empty and parses into a positive integer.
/// 2. If the snapshot metadata records a `last_transaction_xid`, the target XID is not earlier
///    than that baseline transaction (forward recovery cannot roll backward).
///
/// # Arguments
/// * `xid_raw` - Raw string representation of the target XID (e.g. "10050").
/// * `snapshot` - Basebackup metadata for the selected snapshot.
///
/// # Example
/// ```rust
/// use chrono::Utc;
/// use pgvisor_core::backup::{BackupType, BasebackupMeta};
/// use pgvisor_core::backup::validation::validate_recovery_target_xid;
///
/// let snap = BasebackupMeta::new("snap-1", Utc::now(), BackupType::Full, "0/1", 1024)
///     .with_last_transaction(Some(Utc::now()), Some("10000".to_string()));
///
/// // Target equal to or after baseline is valid
/// assert_eq!(validate_recovery_target_xid("10050", &snap).unwrap(), 10050);
///
/// // Target before baseline is rejected
/// assert!(validate_recovery_target_xid("9999", &snap).is_err());
/// ```
pub fn validate_recovery_target_xid(
    xid_raw: &str,
    snapshot: &BasebackupMeta,
) -> Result<u64, BackupError> {
    let trimmed = xid_raw.trim();
    if trimmed.is_empty() {
        return Err(BackupError::InvalidRecoveryTarget(
            "Recovery target XID cannot be empty".to_string(),
        ));
    }

    let parsed_xid = trimmed.parse::<u64>().map_err(|_| {
        BackupError::InvalidRecoveryTarget(format!(
            "Invalid transaction ID '{}'. XID must be a positive integer (e.g. 10050)",
            xid_raw
        ))
    })?;

    if let Some(ref base_xid_str) = snapshot.last_transaction_xid {
        if let Ok(base_xid) = base_xid_str.trim().parse::<u64>() {
            if parsed_xid < base_xid {
                return Err(BackupError::InvalidRecoveryTarget(format!(
                    "Recovery target XID ({}) is earlier than basebackup last transaction XID ({}). PostgreSQL forward recovery cannot roll backward.",
                    parsed_xid, base_xid
                )));
            }
        }
    }

    Ok(parsed_xid)
}

/// Unified validator for any [`super::recovery_target::RecoveryTarget`].
///
/// # Arguments
/// * `target` - The recovery target specification (Time, Xid, or Latest).
/// * `snapshot` - Basebackup metadata for the selected snapshot.
/// * `all_backups` - Slice of all available basebackup metadata records.
/// * `now` - Current reference time for future-timestamp detection.
pub fn validate_recovery_target(
    target: &super::recovery_target::RecoveryTarget,
    snapshot: &BasebackupMeta,
    all_backups: &[BasebackupMeta],
    now: DateTime<Utc>,
) -> Result<(), BackupError> {
    match target {
        super::recovery_target::RecoveryTarget::Time(t) => {
            validate_recovery_target_time(t, snapshot, all_backups, now)?;
            Ok(())
        }
        super::recovery_target::RecoveryTarget::Xid(x) => {
            validate_recovery_target_xid(x, snapshot)?;
            Ok(())
        }
        super::recovery_target::RecoveryTarget::Latest => Ok(()),
    }
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

    #[test]
    fn test_validate_recovery_target_xid() {
        let now = Utc::now();
        let snap = BasebackupMeta::new(
            "snap-1",
            now,
            BackupType::Full,
            "000000010000000000000001",
            1024,
        )
        .with_last_transaction(Some(now), Some("10000".to_string()));

        // Valid: equal or newer XID
        assert_eq!(validate_recovery_target_xid("10000", &snap).unwrap(), 10000);
        assert_eq!(validate_recovery_target_xid("10050", &snap).unwrap(), 10050);

        // Invalid: earlier XID than basebackup
        let err = validate_recovery_target_xid("9999", &snap).unwrap_err();
        match err {
            BackupError::InvalidRecoveryTarget(msg) => {
                assert!(msg.contains("earlier than basebackup last transaction XID"));
                assert!(msg.contains("cannot roll backward"));
            }
            _ => panic!("Expected InvalidRecoveryTarget, got {:?}", err),
        }

        // Invalid: non-numeric or empty
        assert!(validate_recovery_target_xid("", &snap).is_err());
        assert!(validate_recovery_target_xid("not-a-number", &snap).is_err());
    }

    #[test]
    fn test_validate_recovery_target_unified() {
        let now = Utc::now();
        let snap = BasebackupMeta::new(
            "snap-1",
            now - Duration::hours(1),
            BackupType::Full,
            "000000010000000000000001",
            1024,
        )
        .with_last_transaction(Some(now - Duration::hours(1)), Some("5000".to_string()));

        let target_latest = crate::backup::RecoveryTarget::Latest;
        assert!(validate_recovery_target(&target_latest, &snap, &[snap.clone()], now).is_ok());

        let target_xid = crate::backup::RecoveryTarget::Xid("5010".to_string());
        assert!(validate_recovery_target(&target_xid, &snap, &[snap.clone()], now).is_ok());

        let target_xid_invalid = crate::backup::RecoveryTarget::Xid("4999".to_string());
        assert!(validate_recovery_target(&target_xid_invalid, &snap, &[snap.clone()], now).is_err());
    }

    #[test]
    fn test_parse_target_timestamp_supports_utc_suffix() {
        let now = Utc::now();
        let target_str = (now - Duration::minutes(10)).format("%Y-%m-%d %H:%M:%S UTC").to_string();
        let parsed = parse_target_timestamp_with_now(&target_str, now);
        assert!(parsed.is_ok());

        let target_str_parens = (now - Duration::minutes(10)).format("%Y-%m-%d %H:%M:%S (UTC)").to_string();
        let parsed2 = parse_target_timestamp_with_now(&target_str_parens, now);
        assert!(parsed2.is_ok());
    }
}
