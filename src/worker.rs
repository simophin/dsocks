use std::time::Duration;

use anyhow::{bail, Context};
use bytes::Bytes;
use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc,
    task::{spawn_local, LocalSet},
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    connect_async, tungstenite::Error as WsError, tungstenite::Message as WsMessage,
};

use crate::job::{JobRequest, JobResponse, TcpRequest};

const JOB_HEADER_NAME: &str = "X-PJob";
const PING_INTERVAL: Duration = Duration::from_secs(30);

pub async fn serve_worker<T>(worker_name: String, url: &str) -> anyhow::Result<()> {
    let (mut stream, response) = connect_async(url)
        .await
        .context("Connecting to websocket")?;

    log::info!("Connected to {url}");

    let job: JobRequest = if let Some(job) = response.headers().get(JOB_HEADER_NAME) {
        bincode::deserialize(job.as_bytes()).context("Deserialize job in the header")?
    } else {
        wait_until_job_message(&worker_name, &mut stream)
            .await
            .with_context(|| format!("Waiting for job message from {url}"))?
    };

    log::info!("{worker_name}: Starting job {job:?}");

    match job {
        JobRequest::Tcp(TcpRequest {
            addr,
            intial_data: initial_data,
        }) => {
            log::info!("{worker_name}: Connecting to tcp server at {addr}");
            match TcpStream::connect(&addr)
                .await
                .with_context(|| format!("Connecting to tcp server at {addr}"))
            {
                Ok(upstream) => {
                    stream
                        .send(
                            JobResponse::Ok
                                .try_into()
                                .context("Converting to ws message")?,
                        )
                        .await
                        .context("Sending ok message")?;

                    copy_stream(&worker_name, initial_data, stream, upstream).await
                }

                Err(err) => {
                    stream
                        .send(
                            JobResponse::Err(format!("{err:?}"))
                                .try_into()
                                .context("Converting to ws message")?,
                        )
                        .await
                        .context("Sending error message")?;

                    return Err(err);
                }
            }
        }
    }
}

impl TryFrom<JobResponse> for WsMessage {
    type Error = bincode::Error;

    fn try_from(value: JobResponse) -> Result<Self, Self::Error> {
        let bytes = bincode::serialize(&value)?;
        Ok(WsMessage::Binary(bytes))
    }
}

enum ReceivedMessage<U, M> {
    Upstream(U),
    Stream(M),
    ShouldPing,
}

async fn copy_stream<S>(
    worker_name: &str,
    initial_data: Option<Bytes>,
    stream: S,
    upstream: TcpStream,
) -> anyhow::Result<()>
where
    S: Stream<Item = Result<WsMessage, WsError>> + Unpin + Sink<WsMessage> + 'static,
    <S as Sink<WsMessage>>::Error: std::error::Error + Send + Sync + 'static,
{
    log::info!("{worker_name}: Starting streaming");
    LocalSet::default()
        .run_until(async move {
            let (stream_tx, mut stream_rx) = stream.split();
            let (mut upstream_rx, upstream_tx) = upstream.into_split();

            let (stream_sender, stream_receiver) = mpsc::channel(10);
            let (upstream_sender, upstream_receiver) = mpsc::channel(10);

            let stream_job = spawn_local(write_messages(stream_receiver, stream_tx));
            let upstream_job =
                spawn_local(write_data(upstream_receiver, upstream_tx, initial_data));

            let mut upstream_buf = vec![0u8; 4096];

            loop {
                let received = select! {
                    len = upstream_rx.read(&mut upstream_buf) => ReceivedMessage::Upstream(len),
                    msg = stream_rx.next() => ReceivedMessage::Stream(msg),
                    _ = sleep(PING_INTERVAL) => ReceivedMessage::ShouldPing,
                };

                match received {
                    ReceivedMessage::Upstream(Ok(len)) => {
                        if let Err(e) = stream_sender
                            .send(WsMessage::Binary((&upstream_buf[..len]).to_vec()))
                            .await
                        {
                            log::error!("{worker_name}: Error sending to stream: {e:?}. Stopping");
                            break;
                        }
                    }

                    ReceivedMessage::Upstream(Err(e)) => {
                        log::error!("{worker_name}: Error receiving from stream: {e:?}");
                        break;
                    }

                    ReceivedMessage::Stream(Some(Ok(WsMessage::Binary(msg)))) => {
                        if let Err(e) = upstream_sender.send(msg).await {
                            log::error!(
                                "{worker_name}: Error sending to upstream: {e:?}. Stopping"
                            );
                            break;
                        }
                    }

                    ReceivedMessage::Stream(Some(Ok(WsMessage::Ping(_)))) => {
                        log::info!("{worker_name}: Received ping and sending pong");
                        if let Err(e) = stream_sender
                            .send(WsMessage::Pong(Default::default()))
                            .await
                        {
                            log::error!("{worker_name}: Error sending pong: {e:?}. Stopping");
                            break;
                        }
                    }

                    ReceivedMessage::Stream(Some(Ok(WsMessage::Pong(_)))) => {
                        log::info!("{worker_name}: Received pong");
                    }

                    ReceivedMessage::Stream(Some(Ok(WsMessage::Close(_)))) => {
                        log::info!("{worker_name}: Received close");
                        break;
                    }

                    ReceivedMessage::Stream(Some(Ok(_))) => {
                        log::error!("{worker_name}: Received unknown WsMessage type");
                        break;
                    }

                    ReceivedMessage::Stream(Some(Err(err))) => {
                        log::error!("{worker_name}: Error receiving WsMessage: {err:?}");
                        break;
                    }

                    ReceivedMessage::Stream(None) => break,

                    ReceivedMessage::ShouldPing => {
                        log::info!("{worker_name}: Sending ping");
                        if stream_sender
                            .send(WsMessage::Ping(Default::default()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }

            log::info!("{worker_name}: Streaming finished");

            let _ = stream_job.await;
            let _ = upstream_job.await;

            Ok(())
        })
        .await
}

async fn write_messages<S>(mut rx: mpsc::Receiver<WsMessage>, mut tx: S) -> anyhow::Result<()>
where
    S: Sink<WsMessage> + Unpin,
    <S as Sink<WsMessage>>::Error: std::error::Error + Send + Sync + 'static,
{
    while let Some(msg) = rx.recv().await {
        tx.send(msg).await.context("Sending data to websocket")?;
    }

    Ok(())
}

async fn write_data<S>(
    mut rx: mpsc::Receiver<Vec<u8>>,
    mut tx: S,
    initial_data: Option<Bytes>,
) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    if let Some(data) = initial_data {
        tx.write_all(&data)
            .await
            .context("Sending initial data to upstream")?;
    }

    while let Some(data) = rx.recv().await {
        tx.write_all(&data)
            .await
            .context("Sending data to upstream")?;
    }

    Ok(())
}

async fn wait_until_job_message<S>(worker_name: &str, stream: &mut S) -> anyhow::Result<JobRequest>
where
    S: Stream<Item = Result<WsMessage, WsError>> + Unpin + Sink<WsMessage>,
    <S as Sink<WsMessage>>::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        match timeout(PING_INTERVAL, stream.next()).await {
            Ok(Some(Ok(WsMessage::Binary(msg)))) => {
                let job: JobRequest =
                    bincode::deserialize(&msg).context("Deserialize job in the first WsMessage")?;
                return Ok(job);
            }

            Ok(Some(Ok(WsMessage::Ping(_)))) => {
                log::info!("{worker_name}: Received ping and sending pong");
                stream
                    .send(WsMessage::Pong(Default::default()))
                    .await
                    .context("Sending pong")?;
            }

            Ok(Some(Ok(WsMessage::Pong(_)))) => {
                log::info!("{worker_name}: Received pong");
            }

            Ok(Some(Ok(WsMessage::Close(_)))) => {
                log::info!("{worker_name}: Received close");
                bail!("Close requested by the server")
            }

            Ok(Some(Ok(_))) => bail!("Received unknown WsMessage type"),

            Ok(Some(Err(e))) => bail!("Error receiving WsMessage: {e:?}"),

            Ok(None) => bail!("Connection closed"),

            Err(_) => {
                log::info!("{worker_name}: Sending ping");
                stream
                    .send(WsMessage::Ping(Default::default()))
                    .await
                    .context("Sending ping")?;
            }
        };
    }
}
