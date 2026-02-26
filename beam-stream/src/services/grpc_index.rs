use async_trait::async_trait;
use tonic::transport::Channel;

use beam_index::proto::ScanLibraryRequest;
use beam_index::proto::index_service_client::IndexServiceClient;
use beam_index::services::index::{IndexError, IndexService};

#[derive(Debug, Clone)]
pub struct GrpcIndexService {
    client: IndexServiceClient<Channel>,
}

impl GrpcIndexService {
    pub async fn connect(url: String) -> Result<Self, tonic::transport::Error> {
        let channel = Channel::from_shared(url)
            .expect("Invalid beam-index URL")
            .connect()
            .await?;
        Ok(Self {
            client: IndexServiceClient::new(channel),
        })
    }
}

#[async_trait]
impl IndexService for GrpcIndexService {
    async fn scan_library(&self, library_id: String) -> Result<u32, IndexError> {
        let response = self
            .client
            .clone()
            .scan_library(ScanLibraryRequest { library_id })
            .await
            .map_err(|s| IndexError::PathNotFound(s.to_string()))?;
        Ok(response.into_inner().files_added)
    }
}
