use std::sync::Weak;

use anyhow::Context;
use axum::extract::ws::WebSocket;
use parking_lot::Mutex;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    spawn,
    sync::{broadcast, mpsc, oneshot},
};

use crate::{
    job::JobRequest,
    stream::{BoxedStream, Stream},
};

pub struct Controller {}

pub struct WorkerInfo {
    pub labels: Vec<String>,
    pub websocket: WebSocket,
}

pub struct JobRequestInfo {
    pub req: JobRequest,
    pub label: Option<String>,
    pub stream: BoxedStream,
}

struct FindJobRequest {
    labels: Vec<String>,
    callback: Mutex<oneshot::Sender<JobRequestInfo>>,
}

enum JobMessage {
    FindJob(Weak<FindJobRequest>),
}

impl Controller {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn on_job_request(&self, job: JobRequestInfo) -> anyhow::Result<()> {
        todo!()
    }

    pub async fn on_worker(&self, worker: WorkerInfo) -> anyhow::Result<()> {
        todo!()
    }
}
