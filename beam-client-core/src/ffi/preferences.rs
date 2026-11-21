//! The viewer's own language preferences (issue #264), which the server reads
//! to name each source's default audio and subtitle track.

use super::BeamClient;
use crate::catalog::LanguagePreferences;
use crate::error::BeamError;

#[uniffi::export(async_runtime = "tokio")]
impl BeamClient {
    /// The signed-in viewer's language preferences.
    ///
    /// Read from the server every time: another device may have changed them.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected, and
    /// the [request failures](BeamClient#request-failures).
    pub async fn language_preferences(&self) -> Result<LanguagePreferences, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let response = self
            .send(&server_id, client.get_language_preferences())
            .await?;
        Ok(LanguagePreferences::from_generated(response.into_inner()))
    }

    /// Replace the signed-in viewer's language preferences, answering them as
    /// the server stored them: each language as the code a track is tagged
    /// with, a language listed twice at its first place.
    ///
    /// # Errors
    ///
    /// Returns [`BeamError::NoActiveServer`] when no server is selected,
    /// a non-retryable [`BeamError::Server`] 422 for a language the server
    /// does not know or a list over its limit, and the
    /// [request failures](BeamClient#request-failures).
    pub async fn set_language_preferences(
        &self,
        preferences: LanguagePreferences,
    ) -> Result<LanguagePreferences, BeamError> {
        let (server_id, client, _) = self.active_context()?;
        let body = preferences.to_generated();
        let response = self
            .send(&server_id, client.set_language_preferences(&body))
            .await?;
        Ok(LanguagePreferences::from_generated(response.into_inner()))
    }
}
