//! Database entity modules
//!
//! These entities map to the database tables created by migrations.

pub mod admin_log;
pub mod applied_nfo;
pub mod collection;
pub mod collection_movie;
pub mod credit;
pub mod device_auth;
pub mod episode;
pub mod file_chapter;
pub mod file_trickplay;
pub mod files;
pub mod genre;
pub mod library;
pub mod library_movie;
pub mod library_show;
pub mod media_stream;
pub mod metadata_enrichment;
pub mod movie;
pub mod movie_entry;
pub mod movie_genre;
pub mod pending_auth;
pub mod person;
pub mod playback_rebuffer_count;
pub mod playback_start_count;
pub mod playback_switch_count;
pub mod season;
pub mod session;
pub mod show;
pub mod show_genre;
pub mod sidecar_subtitle;
pub mod user;
pub mod user_access_policy;
pub mod user_language_preference;
pub mod user_library_access;
pub mod user_list_entry;
pub mod watch_state;
