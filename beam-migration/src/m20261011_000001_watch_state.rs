//! Per-user watched state per title (issue #188).
//!
//! `playback_progress` kept one row per (user, file), so a viewer who
//! switched from one source of a movie to another lost their place, and
//! nothing could say a title had been watched. `watch_state` keeps one row
//! per (user, movie) or (user, episode), with the file last played beside the
//! position, a sticky `completed`, a play count, and when the user last
//! dismissed it from continue-watching.
//!
//! Existing progress is carried over, not dropped. Each user's rows for the
//! files of one title merge into one row:
//!
//! * position, duration and last file come from the newest row, so the title
//!   resumes where the viewer last was -- at the start if that row had
//!   finished it;
//! * `completed` is whether any of them had finished it, and `play_count`
//!   how many had;
//! * `last_played_at` is the newest row's time; a tie goes to the larger
//!   file id, so the merge does not depend on the order rows are read in.
//!
//! A file holding a run of episodes (`last_episode_number`) keys by its first
//! episode, and finishing it finished the run. So, as a report reaching the
//! end of such a file does from now on, each finished row of one also plays
//! the season's other episodes of the run: a play each, and -- unless the
//! episode was played on its own file since -- its place back at the start.
//!
//! A row for a file that belongs to no title cannot be kept -- there is no
//! title to key it by -- and is dropped; a row for a file the indexer has
//! marked missing is kept, as the file may return. A position or duration no
//! player could have produced (negative, infinite, NaN, a duration of zero)
//! is stored as the start and as unknown respectively, and a position past
//! the duration as the duration; the new table's CHECKs refuse the former
//! from then on.
//!
//! `down` recreates `playback_progress` from each row's last file. That is
//! lossy by construction -- the per-file rows the merge collapsed, and every
//! row whose file has since been purged, are not recoverable -- so an
//! operator treats this migration as one-way.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const CREATE: &str = r#"
CREATE TABLE watch_state (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    movie_id uuid REFERENCES movies (id) ON DELETE CASCADE,
    episode_id uuid REFERENCES episodes (id) ON DELETE CASCADE,
    show_id uuid REFERENCES shows (id) ON DELETE CASCADE,
    last_file_id uuid REFERENCES files (id) ON DELETE SET NULL,
    position_secs double precision NOT NULL DEFAULT 0,
    duration_secs double precision,
    completed boolean NOT NULL DEFAULT false,
    play_count integer NOT NULL DEFAULT 0,
    last_played_at timestamptz NOT NULL,
    dismissed_at timestamptz,
    CONSTRAINT chk_watch_state_one_title CHECK ((movie_id IS NULL) <> (episode_id IS NULL)),
    CONSTRAINT chk_watch_state_episode_show CHECK ((episode_id IS NULL) = (show_id IS NULL)),
    CONSTRAINT chk_watch_state_position
        CHECK (position_secs >= 0 AND position_secs < 'Infinity'::double precision),
    CONSTRAINT chk_watch_state_duration
        CHECK (duration_secs IS NULL
               OR (duration_secs > 0 AND duration_secs < 'Infinity'::double precision)),
    CONSTRAINT chk_watch_state_play_count CHECK (play_count >= 0)
)
"#;

/// Continue-watching and history read a user's rows newest first; a show's
/// rows are read together for next-up and detail.
const INDEXES: [&str; 4] = [
    "CREATE UNIQUE INDEX idx_watch_state_user_movie ON watch_state (user_id, movie_id)",
    "CREATE UNIQUE INDEX idx_watch_state_user_episode ON watch_state (user_id, episode_id)",
    "CREATE INDEX idx_watch_state_user_played ON watch_state (user_id, last_played_at DESC, id DESC)",
    "CREATE INDEX idx_watch_state_user_show ON watch_state (user_id, show_id)",
];

/// Merge each user's `playback_progress` rows per title into one
/// `watch_state` row, as the module documentation describes.
///
/// `x >= 0 AND x < 'Infinity'` is false for NaN as well: Postgres orders NaN
/// above every other value, Infinity included.
const COPY: &str = r#"
WITH resolved AS (
    SELECT p.user_id,
           p.file_id,
           p.position_secs,
           p.duration_secs,
           p.completed,
           p.updated_at,
           me.movie_id,
           CASE WHEN me.movie_id IS NULL THEN e.id END AS episode_id,
           CASE WHEN me.movie_id IS NULL THEN se.show_id END AS show_id
      FROM playback_progress p
      JOIN files f ON f.id = p.file_id
      LEFT JOIN movie_entries me ON me.id = f.movie_entry_id
      LEFT JOIN episodes e ON e.id = f.episode_id
      LEFT JOIN seasons se ON se.id = e.season_id
     WHERE me.movie_id IS NOT NULL OR se.show_id IS NOT NULL
),
ranked AS (
    SELECT r.*,
           row_number() OVER (PARTITION BY user_id, movie_id, episode_id
                              ORDER BY updated_at DESC, file_id DESC) AS recency,
           bool_or(completed) OVER title AS any_completed,
           count(*) FILTER (WHERE completed) OVER title AS completions,
           max(updated_at) OVER title AS latest
      FROM resolved r
    WINDOW title AS (PARTITION BY user_id, movie_id, episode_id)
)
INSERT INTO watch_state (id, user_id, movie_id, episode_id, show_id, last_file_id,
                         position_secs, duration_secs, completed, play_count,
                         last_played_at, dismissed_at)
SELECT gen_random_uuid(),
       user_id,
       movie_id,
       episode_id,
       show_id,
       file_id,
       CASE WHEN completed THEN 0
            WHEN NOT (position_secs >= 0 AND position_secs < 'Infinity'::double precision)
                THEN 0
            WHEN duration_secs > 0 AND duration_secs < 'Infinity'::double precision
                 AND position_secs > duration_secs
                THEN duration_secs
            ELSE position_secs
       END,
       CASE WHEN duration_secs > 0 AND duration_secs < 'Infinity'::double precision
                THEN duration_secs
       END,
       any_completed,
       completions,
       latest,
       NULL
  FROM ranked
 WHERE recency = 1
"#;

/// Play the rest of each finished multi-episode file's run: the episodes of
/// its season numbered after the file's own, up to its last. As
/// `WatchStateRepository::mark_played` does at runtime, an episode gains a
/// played row at the start, or its existing row is played and counts a play;
/// its place goes back to the start unless the viewer played it since.
const SPREAD: &str = r#"
WITH spread AS (
    SELECT p.user_id,
           x.id AS episode_id,
           se.show_id,
           count(*) AS completions,
           max(p.updated_at) AS latest
      FROM playback_progress p
      JOIN files f ON f.id = p.file_id
      JOIN episodes e ON e.id = f.episode_id
      JOIN seasons se ON se.id = e.season_id
      JOIN episodes x ON x.season_id = e.season_id
                     AND x.episode_number > e.episode_number
                     AND x.episode_number <= f.last_episode_number
     WHERE p.completed
       AND f.movie_entry_id IS NULL
     GROUP BY p.user_id, x.id, se.show_id
)
INSERT INTO watch_state (id, user_id, episode_id, show_id, position_secs, completed, play_count,
                         last_played_at)
SELECT gen_random_uuid(), user_id, episode_id, show_id, 0, true, completions, latest
  FROM spread
ON CONFLICT (user_id, episode_id) DO UPDATE SET
    completed = true,
    play_count = watch_state.play_count + excluded.play_count,
    position_secs = CASE WHEN excluded.last_played_at >= watch_state.last_played_at THEN 0
                         ELSE watch_state.position_secs END,
    last_played_at = greatest(watch_state.last_played_at, excluded.last_played_at)
"#;

/// `playback_progress` as `m20260704_000006` created it.
const RECREATE_PROGRESS: [&str; 3] = [
    r#"
CREATE TABLE playback_progress (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL,
    file_id uuid NOT NULL,
    position_secs double precision NOT NULL,
    duration_secs double precision,
    completed boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL,
    CONSTRAINT fk_playback_progress_user_id FOREIGN KEY (user_id)
        REFERENCES users (id) ON DELETE CASCADE,
    CONSTRAINT fk_playback_progress_file_id FOREIGN KEY (file_id)
        REFERENCES files (id) ON DELETE CASCADE
)
"#,
    "CREATE UNIQUE INDEX idx_playback_progress_user_file ON playback_progress (user_id, file_id)",
    "CREATE INDEX idx_playback_progress_user_updated_at ON playback_progress (user_id, updated_at)",
];

/// Each row back onto the file it last played; a row whose file is gone has
/// nowhere to go. A file reclassified from one title to another can be the
/// last file of two of a user's rows, and the old table holds one row per
/// (user, file): the newer wins.
const COPY_BACK: &str = r#"
INSERT INTO playback_progress (id, user_id, file_id, position_secs, duration_secs, completed,
                               updated_at)
SELECT DISTINCT ON (user_id, last_file_id)
       gen_random_uuid(), user_id, last_file_id, position_secs, duration_secs, completed,
       last_played_at
  FROM watch_state
 WHERE last_file_id IS NOT NULL
 ORDER BY user_id, last_file_id, last_played_at DESC
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(CREATE).await?;
        for index in INDEXES {
            db.execute_unprepared(index).await?;
        }
        db.execute_unprepared(COPY).await?;
        db.execute_unprepared(SPREAD).await?;
        db.execute_unprepared("DROP TABLE playback_progress")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in RECREATE_PROGRESS {
            db.execute_unprepared(statement).await?;
        }
        db.execute_unprepared(COPY_BACK).await?;
        db.execute_unprepared("DROP TABLE watch_state").await?;
        Ok(())
    }
}
