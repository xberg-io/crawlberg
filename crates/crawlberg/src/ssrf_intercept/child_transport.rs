/// The socket of the child-target controller: the three calls that the controller makes on it.
///
/// ~keep For a browser with a debugging port the socket is the WebSocket, and each method is
/// ~keep the call of the same name on it. Only a browser that was launched on a private
/// ~keep debugging pipe has the other socket.
pub(super) trait ChildSocket {
    type Error: std::fmt::Display;

    fn into_cdp_error(error: Self::Error) -> chromiumoxide::error::CdpError;

    fn send(
        &mut self,
        message: async_tungstenite::tungstenite::Message,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Close the socket. The pipe has no close frame and ignores `frame`.
    fn close(
        &mut self,
        frame: Option<async_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn next(
        &mut self,
    ) -> impl Future<Output = Option<Result<async_tungstenite::tungstenite::Message, Self::Error>>> + Send;
}

impl<S> ChildSocket for async_tungstenite::WebSocketStream<S>
where
    S: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send,
{
    type Error = async_tungstenite::tungstenite::Error;

    fn into_cdp_error(error: Self::Error) -> chromiumoxide::error::CdpError {
        chromiumoxide::error::CdpError::Ws(error)
    }

    async fn send(&mut self, message: async_tungstenite::tungstenite::Message) -> Result<(), Self::Error> {
        async_tungstenite::WebSocketStream::send(self, message).await
    }

    async fn close(
        &mut self,
        frame: Option<async_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> Result<(), Self::Error> {
        async_tungstenite::WebSocketStream::close(self, frame).await
    }

    async fn next(&mut self) -> Option<Result<async_tungstenite::tungstenite::Message, Self::Error>> {
        futures::StreamExt::next(self).await
    }
}

#[cfg(unix)]
impl ChildSocket for chromiumoxide::pipe::PipeClient {
    type Error = chromiumoxide::error::CdpError;

    fn into_cdp_error(error: Self::Error) -> chromiumoxide::error::CdpError {
        error
    }

    async fn send(&mut self, message: async_tungstenite::tungstenite::Message) -> Result<(), Self::Error> {
        futures::SinkExt::send(self, message).await
    }

    async fn close(
        &mut self,
        _frame: Option<async_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> Result<(), Self::Error> {
        futures::SinkExt::close(self).await
    }

    async fn next(&mut self) -> Option<Result<async_tungstenite::tungstenite::Message, Self::Error>> {
        futures::StreamExt::next(self).await
    }
}
