//! Database entity modules
//!
//! These entities map to the database tables created by migrations.

pub mod admin_log;
pub mod applied_nfo;
pub mod device_auth;
pub mod episode;
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
pub mod playback_progress;
pub mod playback_rebuffer_count;
pub mod playback_start_count;
pub mod playback_switch_count;
pub mod season;
pub mod session;
pub mod show;
pub mod show_genre;
pub mod sidecar_subtitle;
pub mod user;

pub use admin_log::Entity as AdminLog;
pub use applied_nfo::Entity as AppliedNfo;
pub use device_auth::Entity as DeviceAuth;
pub use episode::Entity as Episode;
pub use files::Entity as Files;
pub use genre::Entity as Genre;
pub use library::Entity as Library;
pub use library_movie::Entity as LibraryMovie;
pub use library_show::Entity as LibraryShow;
pub use media_stream::Entity as MediaStream;
pub use metadata_enrichment::Entity as MetadataEnrichment;
pub use movie::Entity as Movie;
pub use movie_entry::Entity as MovieEntry;
pub use movie_genre::Entity as MovieGenre;
pub use pending_auth::Entity as PendingAuth;
pub use playback_progress::Entity as PlaybackProgress;
pub use playback_rebuffer_count::Entity as PlaybackRebufferCount;
pub use playback_start_count::Entity as PlaybackStartCount;
pub use playback_switch_count::Entity as PlaybackSwitchCount;
pub use season::Entity as Season;
pub use session::Entity as Session;
pub use show::Entity as Show;
pub use show_genre::Entity as ShowGenre;
pub use sidecar_subtitle::Entity as SidecarSubtitle;
pub use user::Entity as User;
