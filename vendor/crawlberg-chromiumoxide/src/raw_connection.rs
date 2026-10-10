use std::pin::Pin;
use std::task::{Context, Poll};

use async_tungstenite::tungstenite::Message;
use async_tungstenite::{WebSocketStream, tokio::ConnectStream};
use futures::{Sink, Stream};

use crate::error::{CdpError, Result};

/// An independent CDP client using the browser's private transport.
#[derive(Debug)]
pub struct RawConnection {
    pub(crate) inner: RawTransport,
}

#[derive(Debug)]
pub(crate) enum RawTransport {
    WebSocket(Box<WebSocketStream<ConnectStream>>),
    #[cfg(unix)]
    Pipe(crate::pipe::PipeClient),
}

impl RawConnection {
    pub async fn connect(url: &str) -> Result<Self> {
        let config = async_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        // ~keep The transport wrapper adds a future layer; bound handshake layout for generated async bindings.
        let (socket, _) = Box::pin(async_tungstenite::tokio::connect_async_with_config(url, Some(config))).await?;
        Ok(Self {
            inner: RawTransport::WebSocket(Box::new(socket)),
        })
    }
}

impl Stream for RawConnection {
    type Item = Result<Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match &mut self.get_mut().inner {
            RawTransport::WebSocket(socket) => Pin::new(socket)
                .poll_next(cx)
                .map(|item| item.map(|result| result.map_err(CdpError::from))),
            #[cfg(unix)]
            RawTransport::Pipe(client) => Pin::new(client).poll_next(cx),
        }
    }
}

impl Sink<Message> for RawConnection {
    type Error = CdpError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match &mut self.get_mut().inner {
            RawTransport::WebSocket(socket) => Pin::new(socket).poll_ready(cx).map_err(CdpError::from),
            #[cfg(unix)]
            RawTransport::Pipe(client) => Pin::new(client).poll_ready(cx),
        }
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<()> {
        match &mut self.get_mut().inner {
            RawTransport::WebSocket(socket) => Pin::new(socket).start_send(message).map_err(CdpError::from),
            #[cfg(unix)]
            RawTransport::Pipe(client) => Pin::new(client).start_send(message),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match &mut self.get_mut().inner {
            RawTransport::WebSocket(socket) => Pin::new(socket).poll_flush(cx).map_err(CdpError::from),
            #[cfg(unix)]
            RawTransport::Pipe(client) => Pin::new(client).poll_flush(cx),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match &mut self.get_mut().inner {
            RawTransport::WebSocket(socket) => Pin::new(socket).poll_close(cx).map_err(CdpError::from),
            #[cfg(unix)]
            RawTransport::Pipe(client) => Pin::new(client).poll_close(cx),
        }
    }
}
