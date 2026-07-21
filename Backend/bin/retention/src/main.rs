//! Fase 8 retention worker. Reads env config; RETENTION_RUN_ONCE=true runs a
//! single cycle and exits (CI/manual), otherwise self-schedules a daily run at
//! RETENTION_SCHEDULE_HOUR:RETENTION_SCHEDULE_MINUTE (local time). All logic is
//! in store::retention; this binary only parses config and drives the loop.
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{Local, NaiveTime};
use sqlx::postgres::PgPoolOptions;
use store::retention::{run_cycle, RetentionConfig, RetentionTable, RETENTION_ADVISORY_KEY};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn build_config() -> RetentionConfig {
    let windows = vec![
        (RetentionTable::Bookings, env_i64("RETENTION_BOOKINGS_DAYS", 90)),
        (RetentionTable::AcceptEvents, env_i64("RETENTION_ACCEPT_EVENTS_DAYS", 180)),
        (RetentionTable::Notifications, env_i64("RETENTION_NOTIFICATIONS_DAYS", 30)),
    ];
    RetentionConfig {
        dry_run: env_or("RETENTION_DRY_RUN", "true") != "false",
        archive_dir: PathBuf::from(env_or("RETENTION_ARCHIVE_DIR", "/archive")),
        delete_batch: env_i64("RETENTION_DELETE_BATCH", 5000).max(1) as usize,
        windows,
        advisory_key: RETENTION_ADVISORY_KEY, // the one fixed production single-runner key
    }
}

/// Seconds from `now` until the next local HH:MM. If today's HH:MM has passed,
/// schedule for tomorrow. Always >= 1s.
fn seconds_until_next(now: chrono::DateTime<Local>, hour: u32, minute: u32) -> u64 {
    let target_time = NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap();
    let today_target = now.date_naive().and_time(target_time);
    let next = if now.time() < target_time {
        today_target
    } else {
        (now.date_naive() + chrono::Duration::days(1)).and_time(target_time)
    };
    let next_local = next.and_local_timezone(Local).earliest().unwrap_or(now);
    (next_local - now).num_seconds().max(1) as u64
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = env_or(
        "DATABASE_URL",
        "postgres://tower:tower_dev_only@127.0.0.1:15432/tower",
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .expect("retention: connect Postgres");

    let config = build_config();
    tracing::info!(dry_run = config.dry_run, archive_dir = %config.archive_dir.display(), "retention worker starting");

    let run_once = env_or("RETENTION_RUN_ONCE", "false") == "true";
    if run_once {
        match run_cycle(&pool, &config).await {
            Ok(outcomes) => {
                for o in &outcomes {
                    tracing::info!(table = o.table.name(), captured = o.captured, archived = o.archived, deleted = o.deleted, status = ?o.status, "retention table done");
                }
                if outcomes.is_empty() {
                    tracing::warn!("retention skipped — another runner holds the advisory lock");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "retention cycle failed");
                std::process::exit(1);
            }
        }
        return;
    }

    let hour = env_i64("RETENTION_SCHEDULE_HOUR", 3).clamp(0, 23) as u32;
    let minute = env_i64("RETENTION_SCHEDULE_MINUTE", 30).clamp(0, 59) as u32;
    loop {
        let wait = seconds_until_next(Local::now(), hour, minute);
        tracing::info!(seconds = wait, "retention sleeping until next run");
        tokio::time::sleep(StdDuration::from_secs(wait)).await;
        match run_cycle(&pool, &config).await {
            Ok(outcomes) => {
                for o in &outcomes {
                    tracing::info!(table = o.table.name(), captured = o.captured, archived = o.archived, deleted = o.deleted, status = ?o.status, "retention table done");
                }
            }
            Err(e) => tracing::error!(error = %e, "retention cycle failed; will retry next schedule"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::seconds_until_next;
    use chrono::{Local, TimeZone};

    #[test]
    fn schedules_later_today_when_target_not_yet_passed() {
        // 01:00 local, target 03:30 → 2h30m = 9000s.
        let now = Local.with_ymd_and_hms(2026, 7, 21, 1, 0, 0).unwrap();
        assert_eq!(seconds_until_next(now, 3, 30), 9000);
    }

    #[test]
    fn schedules_tomorrow_when_target_already_passed() {
        // 04:00 local, target 03:30 → 23h30m tomorrow = 84600s.
        let now = Local.with_ymd_and_hms(2026, 7, 21, 4, 0, 0).unwrap();
        assert_eq!(seconds_until_next(now, 3, 30), 84600);
    }
}
