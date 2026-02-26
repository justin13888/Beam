use confique::Config;

#[derive(Debug, Clone, Config)]
pub struct IndexConfig {
    #[config(
        env = "DATABASE_URL",
        default = "postgres://beam:password@localhost:5432/beam"
    )]
    pub database_url: String,

    #[config(env = "BIND_HOST", default = "0.0.0.0")]
    pub host: String,

    #[config(env = "GRPC_PORT", default = 50051)]
    pub port: u16,
}
