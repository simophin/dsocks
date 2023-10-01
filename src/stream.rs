use tokio::io::{AsyncRead, AsyncWrite};

pub trait Stream: AsyncRead + AsyncWrite + Send + Sync + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Sync + 'static> Stream for T {}

pub type BoxedStream = Box<dyn Stream>;
