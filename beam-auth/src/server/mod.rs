pub mod oidc_routes;

pub use oidc_routes::{
    OidcRuntimeConfig, oidc_callback, oidc_delete_session, oidc_list_sessions, oidc_login,
    oidc_logout, oidc_logout_all, oidc_me,
};
