use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::proto::index_service_server::IndexService as IndexServiceTrait;
use crate::proto::{ScanLibraryRequest, ScanLibraryResponse};
use crate::services::index::{IndexError, IndexService, LocalIndexService};

#[derive(Debug)]
pub struct IndexServiceGrpc {
    inner: Arc<LocalIndexService>,
}

impl IndexServiceGrpc {
    pub fn new(inner: Arc<LocalIndexService>) -> Self {
        Self { inner }
    }
}

#[tonic::async_trait]
impl IndexServiceTrait for IndexServiceGrpc {
    async fn scan_library(
        &self,
        request: Request<ScanLibraryRequest>,
    ) -> Result<Response<ScanLibraryResponse>, Status> {
        let library_id = request.into_inner().library_id;
        match self.inner.scan_library(library_id).await {
            Ok(files_added) => Ok(Response::new(ScanLibraryResponse { files_added })),
            Err(IndexError::LibraryNotFound) => Err(Status::not_found("Library not found")),
            Err(IndexError::InvalidId) => Err(Status::invalid_argument("Invalid library ID")),
            Err(IndexError::PathNotFound(s)) => Err(Status::not_found(s)),
            Err(IndexError::Db(e)) => Err(Status::internal(e.to_string())),
        }
    }
}
