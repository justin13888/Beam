pub mod admin_log;
pub mod file;
pub mod library;
pub mod movie;
pub mod show;
pub mod stream;

// Re-export all repository traits
pub use admin_log::{AdminLogRepository, SqlAdminLogRepository};
pub use file::{FileRepository, SqlFileRepository};
pub use library::{LibraryRepository, SqlLibraryRepository};
pub use movie::{MovieRepository, SqlMovieRepository};
pub use show::{ShowRepository, SqlShowRepository};
pub use stream::{MediaStreamRepository, SqlMediaStreamRepository};
