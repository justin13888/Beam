//! `/v1/me/*`: what the caller sets for themselves (issue #264). `GET
//! /v1/me` itself, the signed-in user, is the auth module's.

use kynos::prelude::*;

use beam_domain::models::language_preferences::{LanguageCode, LanguagePreferences};

use crate::models::{FieldError, LanguagePreferenceSettings};
use crate::routes::api_error::{InternalError, LanguagePreferencesUpdateError, SessionAuth};
use crate::routes::playback::parse_user_id;
use crate::routes::tags::Auth;
use crate::state::AppState;

/// The caller's audio and subtitle language preferences.
///
/// A caller who never set any has none listed and forced subtitles only.
#[kynos::get(
    "/me/language-preferences",
    tag = Auth,
    operation_id = "getLanguagePreferences"
)]
pub async fn get_language_preferences(
    auth: SessionAuth,
    Inject(state): Inject<AppState>,
) -> Result<Json<LanguagePreferenceSettings>, InternalError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let preferences = state
        .services
        .language_preferences
        .find(user_id)
        .await
        .map_err(|err| InternalError::Internal(err.to_string()))?;
    Ok(Json(LanguagePreferenceSettings::from(&preferences)))
}

/// Replace the caller's audio and subtitle language preferences.
///
/// One whole: what is not in the body is not kept. Answers what is now
/// stored, each language as its ISO 639-2/B code. Takes effect on the caller's next `GET /v1/media/{id}/sources`, on every
/// device they are signed in on.
#[kynos::put(
    "/me/language-preferences",
    tag = Auth,
    operation_id = "setLanguagePreferences"
)]
pub async fn set_language_preferences(
    auth: SessionAuth,
    Inject(state): Inject<AppState>,
    Json(body): Json<LanguagePreferenceSettings>,
) -> Result<Json<LanguagePreferenceSettings>, LanguagePreferencesUpdateError> {
    let user_id = parse_user_id(&auth.0.user_id)?;
    let LanguagePreferenceSettings {
        audio_languages,
        subtitle_languages,
        subtitle_mode,
    } = body;

    let mut errors = Vec::new();
    let audio = codes("audio_languages", &audio_languages, &mut errors);
    let subtitle = codes("subtitle_languages", &subtitle_languages, &mut errors);
    // The schema's `maxItems` refuses a list longer than
    // `MAX_PREFERRED_LANGUAGES` as Kynos's own 422 before the handler runs,
    // and a list within it never names more distinct languages than it
    // holds; the domain's bound is the second check.
    match LanguagePreferences::new(audio, subtitle, subtitle_mode.into()) {
        Ok(preferences) if errors.is_empty() => {
            state
                .services
                .language_preferences
                .replace(user_id, &preferences)
                .await
                .map_err(|err| LanguagePreferencesUpdateError::Internal(err.to_string()))?;
            Ok(Json(LanguagePreferenceSettings::from(&preferences)))
        }
        Ok(_) => Err(LanguagePreferencesUpdateError::ValidationFailed { errors }),
        Err(too_many) => {
            errors.push(FieldError {
                pointer: String::new(),
                detail: too_many.to_string(),
            });
            Err(LanguagePreferencesUpdateError::ValidationFailed { errors })
        }
    }
}

/// The languages `raw` names, in order, with a field error for each entry
/// that names none.
fn codes(list: &str, raw: &[String], errors: &mut Vec<FieldError>) -> Vec<LanguageCode> {
    let mut parsed = Vec::with_capacity(raw.len());
    for (index, entry) in raw.iter().enumerate() {
        match LanguageCode::parse(entry) {
            Some(code) => parsed.push(code),
            None => errors.push(FieldError {
                pointer: format!("/{list}/{index}"),
                detail: format!(
                    "{entry:?} is not a language: send an ISO 639 code (`en`, `eng`), \
                     an English name or a BCP 47 tag"
                ),
            }),
        }
    }
    parsed
}

#[cfg(test)]
#[path = "me_tests.rs"]
mod me_tests;
