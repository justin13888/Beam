pub mod config;
pub mod grpc;
pub mod probe;
pub mod repositories;
pub mod services;

pub mod proto {
    tonic::include_proto!("beam_index");
}
