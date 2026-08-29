//! The `/v1` tag vocabulary, as types.
//!
//! Kynos makes a tag a type rather than a string, so a misspelling is a
//! compile error instead of a sixth tag nobody meant to create. The five here
//! are exactly the five the Salvo implementation emitted, spelled the same way,
//! because ADR-0010 preserves the `/v1` contract across the migration.

use kynos::prelude::*;

/// Liveness and dependency probing.
#[derive(Tag)]
#[tag(name = "health", description = "Liveness and dependency probing")]
pub struct Health;

/// The OIDC BFF flow and the sessions it mints (ADR-0003).
#[derive(Tag)]
#[tag(name = "auth", description = "Sign-in, sessions, and the current user")]
pub struct Auth;

/// Browsing and reading the media catalogue.
#[derive(Tag)]
#[tag(name = "media", description = "Discovery, detail, and library contents")]
pub struct Media;

/// Delivery, progress, and history.
#[derive(Tag)]
#[tag(name = "playback", description = "Streaming, download, progress, and history")]
pub struct Playback;

/// Everything gated behind the admin scope.
#[derive(Tag)]
#[tag(name = "admin", description = "Library, user, and system administration")]
pub struct Admin;
