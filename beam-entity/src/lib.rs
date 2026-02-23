//! Database entity modules
//!
//! These entities map to the database tables created by migrations.

pub mod episode;
pub mod files;
pub mod genre;
pub mod library;
pub mod library_movie;
pub mod library_show;
pub mod media_stream;
pub mod movie;
pub mod movie_entry;
pub mod movie_genre;
pub mod season;
pub mod show;
pub mod show_genre;
pub mod stream_cache;
pub mod user;

pub use episode::Entity as Episode;
pub use files::Entity as Files;
pub use genre::Entity as Genre;
pub use library::Entity as Library;
pub use library_movie::Entity as LibraryMovie;
pub use library_show::Entity as LibraryShow;
pub use media_stream::Entity as MediaStream;
pub use movie::Entity as Movie;
pub use movie_entry::Entity as MovieEntry;
pub use movie_genre::Entity as MovieGenre;
pub use season::Entity as Season;
pub use show::Entity as Show;
pub use show_genre::Entity as ShowGenre;
pub use stream_cache::Entity as StreamCache;
pub use user::Entity as User;
