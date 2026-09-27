use sea_orm_migration::prelude::*;

/// Adds the three daily counter tables behind operator-local playback
/// telemetry (issue #143, ADR-0019).
///
/// Counters, not an event log: a row is one UTC day and one combination of
/// coarse dimensions -- client kind, container, codecs, height and bitrate
/// class -- and a number. There is no user, file, title or session column, so
/// no row can be read back as anyone's viewing record; the server resolves a
/// reported file to its dimensions and drops the file id before anything is
/// written.
///
/// Every dimension is `NOT NULL`, with a sentinel (`none`, `unknown`) where a
/// value is absent, because the dimensions together are the primary key and
/// the `ON CONFLICT` target the ingest increments through: a nullable key
/// column would never conflict, and each report would insert a new row.
/// `day` leads every key, so the retention prune and the report's date range
/// are both a prefix scan.
///
/// The vocabularies are deliberately not `CHECK`ed here: a new client kind or
/// failure reason is a code change, and should not also need a migration. What
/// is checked is what no code change can make valid -- a negative counter, and
/// a successful start that carries a failure reason or stage.
#[derive(DeriveMigrationName)]
pub struct Migration;

/// One statement per table, executed one at a time.
const UP: [&str; 3] = [
    "CREATE TABLE playback_start_counts (
    day DATE NOT NULL,
    client_kind TEXT NOT NULL,
    outcome TEXT NOT NULL,
    reason TEXT NOT NULL,
    stage TEXT NOT NULL,
    container TEXT NOT NULL,
    video_codec TEXT NOT NULL,
    audio_codec TEXT NOT NULL,
    height_class TEXT NOT NULL,
    count BIGINT NOT NULL CHECK (count >= 0),
    PRIMARY KEY (day, client_kind, outcome, reason, stage, container, video_codec, audio_codec, height_class),
    CONSTRAINT playback_start_counts_outcome_coherent
        CHECK ((outcome = 'started') = (reason = 'none') AND (outcome = 'started') = (stage = 'none'))
)",
    "CREATE TABLE playback_rebuffer_counts (
    day DATE NOT NULL,
    client_kind TEXT NOT NULL,
    container TEXT NOT NULL,
    video_codec TEXT NOT NULL,
    height_class TEXT NOT NULL,
    bitrate_class TEXT NOT NULL,
    events BIGINT NOT NULL CHECK (events >= 0),
    total_ms BIGINT NOT NULL CHECK (total_ms >= 0),
    lt_1s BIGINT NOT NULL CHECK (lt_1s >= 0),
    s1_3 BIGINT NOT NULL CHECK (s1_3 >= 0),
    s3_10 BIGINT NOT NULL CHECK (s3_10 >= 0),
    s10_30 BIGINT NOT NULL CHECK (s10_30 >= 0),
    ge_30s BIGINT NOT NULL CHECK (ge_30s >= 0),
    PRIMARY KEY (day, client_kind, container, video_codec, height_class, bitrate_class)
)",
    "CREATE TABLE playback_switch_counts (
    day DATE NOT NULL,
    client_kind TEXT NOT NULL,
    trigger TEXT NOT NULL,
    from_height_class TEXT NOT NULL,
    to_height_class TEXT NOT NULL,
    count BIGINT NOT NULL CHECK (count >= 0),
    PRIMARY KEY (day, client_kind, trigger, from_height_class, to_height_class)
)",
];

const DOWN: [&str; 3] = [
    "DROP TABLE IF EXISTS playback_switch_counts",
    "DROP TABLE IF EXISTS playback_rebuffer_counts",
    "DROP TABLE IF EXISTS playback_start_counts",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in UP {
            db.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in DOWN {
            db.execute_unprepared(statement).await?;
        }
        Ok(())
    }
}
