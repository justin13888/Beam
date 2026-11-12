pub mod artwork;
pub mod cameo;
pub mod composite;
#[cfg(test)]
pub(crate) mod contract;
pub mod telemetry;
pub mod tvdb;

pub use artwork::{ArtworkFetchLimits, ReqwestArtworkFetcher};
pub use cameo::{CameoEnrichmentProvider, CameoWiringConfig, build_client};
pub use composite::CompositeEnrichmentProvider;
pub use telemetry::ReqwestTelemetrySink;
pub use tvdb::{TvdbEnrichmentProvider, TvdbWiringConfig};
