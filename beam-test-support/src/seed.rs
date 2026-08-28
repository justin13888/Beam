//! Row seeding for the `pg-integration` tier.
//!
//! A real Postgres enforces the foreign keys the in-memory doubles do not, so a
//! test that wants a `playback_progress` row first needs a `users` row and a
//! `files` row -- and a `files` row needs a library, a movie, and a movie entry
//! behind it. These helpers insert the minimum chain, with fresh identifiers on
//! every call so concurrently-running tests never observe one another's rows.

use sea_orm::{ActiveModelTrait, DatabaseConnection, DbErr, Set};
use uuid::Uuid;

fn now() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::Utc::now().into()
}

/// Insert a user and return its id.
pub async fn user(db: &DatabaseConnection) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    beam_entity::user::ActiveModel {
        id: Set(id),
        // Unique together with `oidc_subject`; a fresh id in both keeps
        // parallel tests from colliding on the JIT-provisioning key.
        oidc_issuer: Set(format!("https://idp.test/{id}")),
        oidc_subject: Set(id.to_string()),
        email: Set(None),
        display_name: Set("pg-integration user".to_string()),
        avatar_url: Set(None),
        is_admin: Set(false),
        disabled: Set(false),
        created_at: Set(now()),
        updated_at: Set(now()),
    }
    .insert(db)
    .await?;
    Ok(id)
}

/// Insert a library and return its id.
pub async fn library(db: &DatabaseConnection) -> Result<Uuid, DbErr> {
    let id = Uuid::new_v4();
    beam_entity::library::ActiveModel {
        id: Set(id),
        name: Set(format!("library-{id}")),
        description: Set(None),
        root_path: Set(format!("/videos/{id}")),
        created_at: Set(now()),
        updated_at: Set(now()),
        last_scan_started_at: Set(None),
        last_scan_finished_at: Set(None),
        last_scan_file_count: Set(None),
    }
    .insert(db)
    .await?;
    Ok(id)
}

/// Insert a movie and a movie entry inside `library_id`, returning the entry's
/// id -- the polymorphic parent a movie file hangs off.
pub async fn movie_entry(db: &DatabaseConnection, library_id: Uuid) -> Result<Uuid, DbErr> {
    let movie_id = Uuid::new_v4();
    beam_entity::movie::ActiveModel {
        id: Set(movie_id),
        title: Set(format!("movie-{movie_id}")),
        title_localized: Set(None),
        description: Set(None),
        year: Set(None),
        release_date: Set(None),
        runtime_mins: Set(None),
        poster_url: Set(None),
        backdrop_url: Set(None),
        tmdb_id: Set(None),
        imdb_id: Set(None),
        tvdb_id: Set(None),
        anilist_id: Set(None),
        rating_tmdb: Set(None),
        rating_imdb: Set(None),
        created_at: Set(now()),
        updated_at: Set(now()),
    }
    .insert(db)
    .await?;

    let entry_id = Uuid::new_v4();
    beam_entity::movie_entry::ActiveModel {
        id: Set(entry_id),
        library_id: Set(library_id),
        movie_id: Set(movie_id),
        edition: Set(None),
        is_primary: Set(true),
        created_at: Set(now()),
    }
    .insert(db)
    .await?;

    Ok(entry_id)
}

/// Insert a media file, plus the library / movie / movie-entry chain it hangs
/// off, and return the file's id.
pub async fn file(db: &DatabaseConnection) -> Result<Uuid, DbErr> {
    let library_id = library(db).await?;
    let entry_id = movie_entry(db, library_id).await?;

    let file_id = Uuid::new_v4();
    beam_entity::files::ActiveModel {
        id: Set(file_id),
        movie_entry_id: Set(Some(entry_id)),
        episode_id: Set(None),
        library_id: Set(library_id),
        file_path: Set(format!("/videos/{library_id}/{file_id}.mkv")),
        file_size: Set(1024),
        mime_type: Set(Some("video/x-matroska".to_string())),
        hash_xxh3: Set(0),
        duration_secs: Set(Some(100.0)),
        container_format: Set(Some("matroska".to_string())),
        language: Set(None),
        quality: Set(None),
        release_group: Set(None),
        is_primary: Set(true),
        scanned_at: Set(now()),
        updated_at: Set(now()),
        file_status: Set(beam_entity::files::FileStatus::Known),
        mtime: Set(None),
    }
    .insert(db)
    .await?;

    Ok(file_id)
}
