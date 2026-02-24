use confique::Config;

/// Application configuration
#[derive(Debug, Clone, Config)]
pub struct ServerConfig {
    #[config(env = "BIND_ADDRESS", default = "0.0.0.0:8001")]
    pub bind_address: String,

    #[config(env = "SERVER_URL", default = "http://localhost:8001")]
    pub server_url: String,

    #[config(
        env = "DATABASE_URL",
        default = "postgres://beam:password@localhost:5432/beam"
    )]
    pub database_url: String,

    #[config(env = "REDIS_URL", default = "redis://localhost:6379")]
    pub redis_url: String,

    #[config(env = "JWT_SECRET")]
    pub jwt_secret: String,
}

impl ServerConfig {
    /// Load configuration from environment variables
    pub fn load_and_validate() -> Result<Self, confique::Error> {
        Self::builder().env().load()
    }
}
