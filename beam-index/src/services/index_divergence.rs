//! Duplicate and runtime-divergence warnings: renditions of one title
//! that share content, or whose probed runtimes disagree (issue #88).

use super::*;

/// Render a runtime in whole minutes for human-facing warnings (e.g. `40 min`).
pub(super) fn humanize_minutes(secs: f64) -> String {
    format!("{} min", (secs / 60.0).round() as i64)
}

/// Thresholds for flagging renditions of the same title whose probed runtimes
/// disagree by enough to suggest a misnamed or mismatched file (issue #88).
///
/// A pair of renditions is treated as *divergent* only when BOTH conditions
/// hold: the relative difference `|a - b| / max(a, b)` exceeds
/// `max_runtime_ratio` AND the absolute difference exceeds
/// `min_runtime_delta_secs`. The double condition keeps the check quiet in the
/// two cases where a single threshold is noisy -- on short content, where a
/// large ratio can still be only a few seconds, and on long content, where a
/// few minutes of legitimate edition drift is a tiny ratio.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DivergencePolicy {
    /// Minimum relative runtime difference (`0.0`–`1.0`) for a pair to count as
    /// diverging. Defaults to `0.15` (15%).
    pub max_runtime_ratio: f64,
    /// Minimum absolute runtime difference, in seconds, for a pair to count as
    /// diverging. Defaults to `240.0` (4 minutes).
    pub min_runtime_delta_secs: f64,
}

impl Default for DivergencePolicy {
    fn default() -> Self {
        Self {
            max_runtime_ratio: 0.15,
            min_runtime_delta_secs: 240.0,
        }
    }
}

impl LocalIndexService {
    /// Report files that share content (same XXH3 hash) with `file`.
    pub(super) async fn check_and_report_duplicate(&self, file: &MediaFile) {
        if file.hash == 0 {
            return; // unhashed sentinel
        }
        let matches = match self.file_repo.find_by_hash(file.hash).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Duplicate check failed for {}: {}", file.path.display(), e);
                return;
            }
        };
        let duplicates: Vec<String> = matches
            .into_iter()
            .filter(|f| f.id != file.id)
            .map(|f| f.path.display().to_string())
            .collect();
        if duplicates.is_empty() {
            return;
        }

        let message = format!(
            "Duplicate content detected: '{}' shares its hash with {} other file(s)",
            file.path.display(),
            duplicates.len()
        );
        self.notification_service.publish(NewAdminEvent::info(
            EventCategory::LibraryScan,
            message.clone(),
            Some(file.library_id),
            None,
        ));
        let _ = self
            .admin_log
            .create(CreateAdminLog {
                level: AdminLogLevel::Info,
                category: AdminLogCategory::LibraryScan,
                message,
                details: Some(serde_json::json!({
                    "file": file.path.display().to_string(),
                    "duplicates": duplicates,
                })),
            })
            .await;
    }

    /// Warn when `file` maps to a movie/episode that already has other files
    /// whose probed runtime disagrees beyond [`DivergencePolicy`]'s thresholds
    /// -- usually a sign of a misnamed or mismatched file (issue #88). Never
    /// fails the scan: like [`Self::check_and_report_duplicate`], every
    /// repository error is swallowed with a `warn!` and the method returns.
    pub(super) async fn check_and_report_runtime_divergence(&self, file: &MediaFile) {
        // Skip files without a usable probed duration -- nothing to compare.
        let Some(duration) = file.duration else {
            return;
        };
        let file_secs = duration.as_secs_f64();
        if file_secs <= 0.0 {
            return;
        }

        // Gather the sibling files that share this file's movie/episode.
        let siblings: Vec<MediaFile> = match &file.content {
            // One part of a multi-part movie lasts that part, which no other
            // file of the movie is expected to match.
            Some(MediaFileContent::Movie {
                part_number: Some(_),
                ..
            }) => return,
            Some(MediaFileContent::Movie {
                movie_entry_id,
                part_number: None,
            }) => {
                let entry = match self.movie_repo.find_entry_by_id(*movie_entry_id).await {
                    Ok(Some(entry)) => entry,
                    Ok(None) => return,
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                };
                let entries = match self
                    .movie_repo
                    .find_entries_by_movie_id(entry.movie_id)
                    .await
                {
                    Ok(entries) => entries,
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                };
                let mut collected = Vec::new();
                for entry in entries {
                    match self.file_repo.find_by_movie_entry_id(entry.id).await {
                        Ok(files) => collected.extend(files.into_iter().filter(|sibling| {
                            matches!(
                                sibling.content,
                                Some(MediaFileContent::Movie {
                                    part_number: None,
                                    ..
                                })
                            )
                        })),
                        Err(e) => {
                            warn!(
                                "Runtime-divergence check failed for {}: {}",
                                file.path.display(),
                                e
                            );
                            return;
                        }
                    }
                }
                collected
            }
            Some(MediaFileContent::Episode {
                episode_id,
                last_episode_number,
            }) => {
                // A multi-episode file and a single episode share a first
                // episode but not a runtime: compare like with like.
                match self.file_repo.find_by_episode_id(*episode_id).await {
                    Ok(files) => files
                        .into_iter()
                        .filter(|sibling| {
                            matches!(
                                &sibling.content,
                                Some(MediaFileContent::Episode {
                                    last_episode_number: sibling_last,
                                    ..
                                }) if sibling_last == last_episode_number
                            )
                        })
                        .collect(),
                    Err(e) => {
                        warn!(
                            "Runtime-divergence check failed for {}: {}",
                            file.path.display(),
                            e
                        );
                        return;
                    }
                }
            }
            None => return,
        };

        // Compare against every sibling that carries a probed runtime, keeping
        // the pairs that diverge past both thresholds.
        let policy = &self.divergence_policy;
        let mut diverging: Vec<(&MediaFile, f64)> = Vec::new();
        for sibling in &siblings {
            if sibling.id == file.id {
                continue;
            }
            let Some(sibling_duration) = sibling.duration else {
                continue;
            };
            let sibling_secs = sibling_duration.as_secs_f64();
            if sibling_secs <= 0.0 {
                continue;
            }
            let delta = (file_secs - sibling_secs).abs();
            let ratio = delta / file_secs.max(sibling_secs);
            if ratio > policy.max_runtime_ratio && delta > policy.min_runtime_delta_secs {
                diverging.push((sibling, sibling_secs));
            }
        }
        if diverging.is_empty() {
            return;
        }

        // Name the sibling that diverges most (largest absolute delta) in the
        // single warning we emit.
        let (worst_sibling, worst_secs) = diverging
            .iter()
            .max_by(|(_, a), (_, b)| (file_secs - *a).abs().total_cmp(&(file_secs - *b).abs()))
            .copied()
            .expect("diverging is non-empty");

        let message = format!(
            "Runtime mismatch: '{}' ({}) differs from '{}' ({}); {} rendition(s) of the same title \
             diverge -- likely a misnamed or mismatched file",
            file.path.display(),
            humanize_minutes(file_secs),
            worst_sibling.path.display(),
            humanize_minutes(worst_secs),
            diverging.len(),
        );

        self.notification_service.publish(NewAdminEvent::warning(
            EventCategory::LibraryScan,
            message.clone(),
            Some(file.library_id),
            None,
        ));
        let siblings_json: Vec<serde_json::Value> = diverging
            .iter()
            .map(|(sibling, secs)| {
                serde_json::json!({
                    "path": sibling.path.display().to_string(),
                    "duration_secs": secs,
                })
            })
            .collect();
        let _ = self
            .admin_log
            .create(CreateAdminLog {
                level: AdminLogLevel::Warning,
                category: AdminLogCategory::LibraryScan,
                message,
                details: Some(serde_json::json!({
                    "file": file.path.display().to_string(),
                    "siblings": siblings_json,
                    "threshold": {
                        "max_runtime_ratio": policy.max_runtime_ratio,
                        "min_runtime_delta_secs": policy.min_runtime_delta_secs,
                    },
                })),
            })
            .await;
        metrics::counter!("beam_index_divergence_warnings_total").increment(1);
    }
}
