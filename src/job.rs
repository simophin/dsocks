use bytes::Bytes;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpRequest {
    pub addr: String,
    pub intial_data: Option<Bytes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobRequest {
    Tcp(TcpRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobResponse {
    Ok,
    Err(String),
}
