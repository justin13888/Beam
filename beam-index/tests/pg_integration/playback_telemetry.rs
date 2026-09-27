//! The shared `PlaybackTelemetryRepository` contract (issue #143), run
//! against real SQL -- the same assertions as the in-memory instantiation in
//! `beam-domain/src/repositories/playback_telemetry.rs` -- and what only a real
//! Postgres can show: that the upsert is atomic, and that the table refuses
//! the rows no code should write.
//!
//! `summarize` and `prune_before` are global, so each test owns a migrated
//! schema: over a shared database they would see every other test's rows.

use std::sync::Arc;

// `start_key` is brought into scope by the contract macro below.
use beam_domain::repositories::PlaybackTelemetryRepository;
use beam_index::repositories::SqlPlaybackTelemetryRepository;
use beam_test_support::postgres::ScopedSchema;
use sea_orm::{ConnectionTrait, Statement};

struct PgFixture {
    // Held so the schema outlives the repository built on it.
    _schema: ScopedSchema,
    db: Arc<sea_orm::DatabaseConnection>,
    repo: SqlPlaybackTelemetryRepository,
}

impl beam_domain::repositories::contract::fixture::PlaybackTelemetryFixture for PgFixture {
    fn repo(&self) -> &dyn PlaybackTelemetryRepository {
        &self.repo
    }
}

async fn setup() -> PgFixture {
    // Left behind on purpose: the fixture cannot await a drop, and the next
    // run's `migrate_once` sweeps every `beam_test_*` schema.
    let schema = ScopedSchema::create_migrated("playback_telemetry_contract")
        .await
        .expect("create a migrated schema");
    let db = schema.db();
    PgFixture {
        repo: SqlPlaybackTelemetryRepository::new(db.clone()),
        db,
        _schema: schema,
    }
}

beam_domain::playback_telemetry_repository_contract!(setup);

/// Why every write is one `ON CONFLICT` statement: eight reports of one key
/// arriving at once must count eight, not fail on the primary key or lose an
/// increment to a read-modify-write race.
#[tokio::test]
async fn concurrent_reports_of_one_key_all_count() {
    let fixture = setup().await;
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 27).unwrap();

    let writes = (0..8).map(|_| fixture.repo.record_start(day, start_key()));
    for result in futures::future::join_all(writes).await {
        result.expect("every concurrent upsert succeeds");
    }

    let summary = fixture.repo.summarize(day, day).await.unwrap();
    assert_eq!(summary.starts.len(), 1);
    assert_eq!(summary.starts[0].count, 8);
}

/// The table refuses a row no outcome describes -- a successful start with a
/// failure reason, a failed one without -- and a negative counter.
#[tokio::test]
async fn the_start_table_refuses_incoherent_rows() {
    let fixture = setup().await;
    let insert = |outcome: &str, reason: &str, stage: &str, count: i64| {
        Statement::from_sql_and_values(
            fixture.db.get_database_backend(),
            "INSERT INTO playback_start_counts \
             (day, client_kind, outcome, reason, stage, container, video_codec, audio_codec, \
              height_class, count) \
             VALUES ('2026-09-27', 'android', $1, $2, $3, 'mp4', 'h264', 'aac', 'hd', $4)",
            [outcome.into(), reason.into(), stage.into(), count.into()],
        )
    };

    fixture
        .db
        .execute_raw(insert("failed", "network", "playback", 1))
        .await
        .expect("a coherent failed row inserts");
    for (outcome, reason, stage, count) in [
        ("started", "network", "none", 1),
        ("started", "none", "playback", 1),
        ("failed", "none", "none", 1),
        ("started", "none", "none", -1),
    ] {
        assert!(
            fixture
                .db
                .execute_raw(insert(outcome, reason, stage, count))
                .await
                .is_err(),
            "{outcome}/{reason}/{stage} count {count} must be refused"
        );
    }
}
