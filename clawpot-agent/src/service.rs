use crate::exec;
use crate::proto::{
    agent_service_server::AgentService, ExecRequest, ExecResponse, ExecStreamInput,
    ExecStreamOutput, HealthRequest, HealthResponse,
};
use crate::stream;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;

pub struct AgentServiceImpl {
    started_at: Instant,
}

impl AgentServiceImpl {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

#[tonic::async_trait]
impl AgentService for AgentServiceImpl {
    async fn exec(&self, request: Request<ExecRequest>) -> Result<Response<ExecResponse>, Status> {
        let req = request.into_inner();
        info!("Exec: {} {:?}", req.command, req.args);

        let response = exec::run_command(req)
            .await
            .map_err(|e| Status::internal(format!("Execution failed: {e}")))?;

        Ok(Response::new(response))
    }

    type ExecStreamStream = ReceiverStream<Result<ExecStreamOutput, Status>>;

    async fn exec_stream(
        &self,
        request: Request<tonic::Streaming<ExecStreamInput>>,
    ) -> Result<Response<Self::ExecStreamStream>, Status> {
        let input_stream = request.into_inner();
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            stream::run_stream(input_stream, tx).await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: self.started_at.elapsed().as_secs(),
        }))
    }
}
