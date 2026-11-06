use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The log filter the server runs with when `RUST_LOG` is unset.
///
/// Every Beam crate the server links, at `info`, plus Kynos: the four event
/// classes NFR-403 names are emitted across them -- scans and enrichment in
/// `beam_index`, sign-in outcomes in `beam_server` and `beam_auth`, admin
/// mutations in `beam_index`'s admin log helper. A crate left out of this list
/// is a crate whose events a default deployment never shows.
///
/// `compose.beam.yaml` and the Helm chart's `server.logLevel` carry this same
/// string, so a deployment that sets nothing logs what one that runs the
/// binary bare does. A test below holds the three together.
pub const DEFAULT_LOG_FILTER: &str = "beam_server=info,beam_auth=info,beam_domain=info,beam_entity=info,beam_index=info,beam_migration=info,kynos=info";

fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER))
}

#[inline]
pub fn init_tracing() {
    #[cfg(debug_assertions)]
    {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .pretty()
                    .with_target(true)
                    .with_file(true)
                    .with_line_number(true)
                    .with_thread_ids(true)
                    .with_thread_names(true),
            )
            .with(env_filter())
            .init();
    }
    #[cfg(not(debug_assertions))]
    {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_target(true)
                    .with_level(true),
            )
            .with(env_filter())
            .init();
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tracing::Level;
    use tracing_subscriber::filter::Targets;

    use super::DEFAULT_LOG_FILTER;

    /// Every `beam-*` crate this one depends on, by its Rust name, read from
    /// the manifest rather than listed again here.
    fn linked_beam_crates() -> Vec<String> {
        let manifest = include_str!("../Cargo.toml");
        let dependencies = manifest
            .split("\n[dependencies]\n")
            .nth(1)
            .and_then(|rest| rest.split("\n[").next())
            .expect("the manifest has a [dependencies] table");
        dependencies
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
            .filter(|name| name.starts_with("beam-"))
            .map(|name| name.replace('-', "_"))
            .collect()
    }

    #[test]
    fn the_default_filter_enables_every_beam_crate() {
        let filter = Targets::from_str(DEFAULT_LOG_FILTER).expect("the default filter parses");
        let mut crates = linked_beam_crates();
        assert!(
            crates.contains(&"beam_index".to_owned()),
            "the manifest was read: {crates:?}"
        );
        crates.push("beam_server".to_owned());
        for name in crates {
            for target in [name.clone(), format!("{name}::some::module")] {
                assert!(
                    filter.would_enable(&target, &Level::INFO),
                    "{target} is not logged at info by default"
                );
            }
        }
        // Kynos reports request failures it answers itself.
        assert!(filter.would_enable("kynos", &Level::INFO));
        // And nothing else is let in: a dependency's own chatter stays out.
        assert!(!filter.would_enable("sqlx", &Level::INFO));
    }

    #[test]
    fn compose_and_the_chart_default_to_the_same_filter() {
        let compose = include_str!("../../compose.beam.yaml");
        assert!(
            compose.contains(&format!("RUST_LOG=${{RUST_LOG:-{DEFAULT_LOG_FILTER}}}")),
            "compose.beam.yaml's RUST_LOG default differs from DEFAULT_LOG_FILTER"
        );
        let values = include_str!("../../charts/beam/values.yaml");
        assert!(
            values.contains(&format!("logLevel: \"{DEFAULT_LOG_FILTER}\"")),
            "charts/beam/values.yaml's server.logLevel differs from DEFAULT_LOG_FILTER"
        );
    }
}
