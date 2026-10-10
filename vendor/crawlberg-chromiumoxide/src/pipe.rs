use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_tungstenite::tungstenite::Message;
use futures::{Sink, Stream};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::error::{CdpError, Result};
use crate::raw_connection::{RawConnection, RawTransport};

const READ_BUFFER_SIZE: usize = 16 * 1024;

fn next_frame_end(bytes: &[u8], scanned: usize) -> Option<usize> {
    bytes[scanned..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|end| scanned + end)
}

#[derive(Debug, Clone)]
pub(crate) struct PipeHub(mpsc::UnboundedSender<Command>);

#[derive(Debug)]
enum Command {
    Send(usize, Value, WriteReceipt),
    Open(oneshot::Sender<Result<RawConnection>>),
    Close(usize, Option<WriteReceipt>),
}

type WriteReceipt = oneshot::Sender<Result<()>>;

struct WriteJob {
    bytes: Vec<u8>,
    receipt: Option<WriteReceipt>,
}

struct WriterTask(tokio::task::JoinHandle<Result<()>>);

impl Drop for WriterTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Debug)]
pub(crate) struct PipeClient {
    id: usize,
    hub: PipeHub,
    incoming: mpsc::UnboundedReceiver<Result<Message>>,
    receipt: Option<oneshot::Receiver<Result<()>>>,
    closing: bool,
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        if !self.closing {
            let _ = self.hub.0.send(Command::Close(self.id, None));
        }
    }
}

impl PipeClient {
    fn poll_receipt(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let Some(receipt) = self.receipt.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let result = std::task::ready!(Pin::new(receipt).poll(cx));
        self.receipt = None;
        Poll::Ready(result.unwrap_or(Err(CdpError::NoResponse)))
    }

    fn queue_close(&mut self) -> Result<()> {
        let (done, receipt) = oneshot::channel();
        self.hub
            .0
            .send(Command::Close(self.id, Some(done)))
            .map_err(|_| CdpError::NoResponse)?;
        self.receipt = Some(receipt);
        self.closing = true;
        Ok(())
    }
}

impl Stream for PipeClient {
    type Item = Result<Message>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().incoming.poll_recv(cx)
    }
}

impl Sink<Message> for PipeClient {
    type Error = CdpError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let client = self.get_mut();
        std::task::ready!(client.poll_receipt(cx))?;
        Poll::Ready(if client.hub.0.is_closed() || client.closing {
            Err(CdpError::NoResponse)
        } else {
            Ok(())
        })
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<()> {
        let client = self.get_mut();
        if client.receipt.is_some() || client.closing {
            return Err(CdpError::NoResponse);
        }
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id").is_none() {
                    return Err(CdpError::NoResponse);
                }
                let (done, receipt) = oneshot::channel();
                client
                    .hub
                    .0
                    .send(Command::Send(client.id, value, done))
                    .map_err(|_| CdpError::NoResponse)?;
                client.receipt = Some(receipt);
                Ok(())
            }
            Message::Close(_) => client.queue_close(),
            other => Err(CdpError::UnexpectedWsMessage(other)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.get_mut().poll_receipt(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let client = self.get_mut();
        std::task::ready!(client.poll_receipt(cx))?;
        if !client.closing {
            client.queue_close()?;
            return client.poll_receipt(cx);
        }
        Poll::Ready(Ok(()))
    }
}

struct ClientState {
    root_session: Option<String>,
    incoming: mpsc::UnboundedSender<Result<Message>>,
}

enum Pending {
    Ignore,
    Reply {
        client: usize,
        original_id: Value,
    },
    Open {
        done: oneshot::Sender<Result<RawConnection>>,
    },
}

struct Router {
    hub: PipeHub,
    clients: HashMap<usize, ClientState>,
    sessions: HashMap<String, usize>,
    pending: HashMap<u64, Pending>,
    next_call: u64,
    next_client: usize,
}

impl PipeHub {
    pub(crate) fn start(socket: tokio::net::UnixStream) -> (Self, RawConnection) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let hub = Self(sender);
        let mut router = Router {
            hub: hub.clone(),
            clients: HashMap::new(),
            sessions: HashMap::new(),
            pending: HashMap::new(),
            next_call: 0,
            next_client: 0,
        };
        let client = router.add_client(None);
        tokio::spawn(async move {
            if let Err(error) = router.run(socket, receiver).await {
                tracing::debug!(%error, "private browser pipe closed");
            }
        });
        (hub, client)
    }

    pub(crate) async fn open_client(&self) -> Result<RawConnection> {
        let (sender, receiver) = oneshot::channel();
        self.0.send(Command::Open(sender)).map_err(|_| CdpError::NoResponse)?;
        receiver.await.map_err(|_| CdpError::NoResponse)?
    }
}

impl Router {
    fn add_client(&mut self, root_session: Option<String>) -> RawConnection {
        let id = self.next_client;
        self.next_client += 1;
        let (incoming, receiver) = mpsc::unbounded_channel();
        if let Some(session) = &root_session {
            self.sessions.insert(session.clone(), id);
        }
        self.clients.insert(id, ClientState { root_session, incoming });
        RawConnection {
            inner: RawTransport::Pipe(PipeClient {
                id,
                hub: self.hub.clone(),
                incoming: receiver,
                receipt: None,
                closing: false,
            }),
        }
    }

    fn encode_command(&mut self, command: Command) -> Result<Option<WriteJob>> {
        let (mut value, pending, receipt) = match command {
            Command::Send(client, mut value, receipt) => {
                let state = self.clients.get(&client).ok_or(CdpError::NoResponse)?;
                if value.get("sessionId").is_none()
                    && let Some(root) = &state.root_session
                {
                    value["sessionId"] = Value::String(root.clone());
                }
                let original_id = value.get("id").cloned().ok_or(CdpError::NoResponse)?;
                (value, Pending::Reply { client, original_id }, Some(receipt))
            }
            Command::Open(done) => (
                serde_json::json!({"method":"Target.attachToBrowserTarget","params":{}}),
                Pending::Open { done },
                None,
            ),
            Command::Close(client, receipt) => {
                let state = self.clients.remove(&client);
                self.sessions.retain(|_, owner| *owner != client);
                let Some(root) = state.and_then(|state| state.root_session) else {
                    if let Some(receipt) = receipt {
                        let _ = receipt.send(Ok(()));
                    }
                    return Ok(None);
                };
                (
                    serde_json::json!({"method":"Target.detachFromTarget","params":{"sessionId":root}}),
                    Pending::Ignore,
                    receipt,
                )
            }
        };
        let id = self.next_call;
        self.next_call += 1;
        value["id"] = Value::from(id);
        self.pending.insert(id, pending);
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(0);
        Ok(Some(WriteJob { bytes, receipt }))
    }

    fn deliver(&mut self, mut value: Value) -> Result<()> {
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            match self.pending.remove(&id) {
                Some(Pending::Reply { client, original_id }) => {
                    value["id"] = original_id;
                    self.send(client, value)?;
                }
                Some(Pending::Open { done }) => {
                    let result = match value.pointer("/result/sessionId").and_then(Value::as_str) {
                        Some(session) => Ok(self.add_client(Some(session.to_owned()))),
                        None => Err(CdpError::msg(format!(
                            "failed to attach a private browser client: {value}"
                        ))),
                    };
                    let _ = done.send(result);
                }
                Some(Pending::Ignore) | None => {}
            }
            return Ok(());
        }
        let session = value.get("sessionId").and_then(Value::as_str);
        let client = match session {
            Some(session) => match self.sessions.get(session) {
                Some(client) => *client,
                None => return Ok(()),
            },
            None => 0,
        };
        if value.get("method").and_then(Value::as_str) == Some("Target.attachedToTarget")
            && let Some(session) = value.pointer("/params/sessionId").and_then(Value::as_str)
        {
            self.sessions.insert(session.to_owned(), client);
        }
        if value.get("method").and_then(Value::as_str) == Some("Target.detachedFromTarget")
            && let Some(session) = value.pointer("/params/sessionId").and_then(Value::as_str)
        {
            self.sessions.remove(session);
        }
        self.send(client, value)
    }

    fn send(&self, client: usize, mut value: Value) -> Result<()> {
        if let Some(state) = self.clients.get(&client) {
            if value.get("sessionId").and_then(Value::as_str) == state.root_session.as_deref()
                && state.root_session.is_some()
            {
                value.as_object_mut().ok_or(CdpError::NoResponse)?.remove("sessionId");
            }
            let message = Message::Text(serde_json::to_string(&value)?.into());
            let _ = state.incoming.send(Ok(message));
        }
        Ok(())
    }

    async fn run(&mut self, socket: tokio::net::UnixStream, commands: mpsc::UnboundedReceiver<Command>) -> Result<()> {
        let result = self.run_inner(socket, commands).await;
        if let Err(error) = &result {
            for state in self.clients.values() {
                let failure = match error {
                    CdpError::Io(error) => std::io::Error::new(error.kind(), error.to_string()),
                    other => std::io::Error::other(other.to_string()),
                };
                let _ = state.incoming.send(Err(CdpError::Io(failure)));
            }
        }
        result
    }

    async fn run_inner(
        &mut self,
        socket: tokio::net::UnixStream,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) -> Result<()> {
        let (mut reader, writer) = socket.into_split();
        let (writes, queued) = mpsc::unbounded_channel();
        let mut writer = WriterTask(tokio::spawn(Self::write_commands(writer, queued)));
        let mut buffer = [0; READ_BUFFER_SIZE];
        let mut pending_bytes = Vec::new();
        let mut scanned = 0;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    let job = self.encode_command(command)?;
                    if self.clients.is_empty() {
                        if let Some(receipt) = job.and_then(|job| job.receipt) { let _ = receipt.send(Ok(())); }
                        return Ok(());
                    }
                    if let Some(job) = job { writes.send(job).map_err(|_| CdpError::NoResponse)?; }
                }
                written = &mut writer.0 => {
                    return written.map_err(|error| CdpError::Io(std::io::Error::other(error)))?;
                }
                read = reader.read(&mut buffer) => {
                    let count = read?;
                    if count == 0 {
                        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "private browser pipe reached EOF").into());
                    }
                    pending_bytes.extend_from_slice(&buffer[..count]);
                    while let Some(end) = next_frame_end(&pending_bytes, scanned) {
                        self.deliver(serde_json::from_slice(&pending_bytes[..end])?)?;
                        pending_bytes.drain(..=end);
                        scanned = 0;
                    }
                    scanned = pending_bytes.len();
                }
            }
        }
    }

    async fn write_commands(
        mut writer: impl tokio::io::AsyncWrite + Unpin,
        mut queued: mpsc::UnboundedReceiver<WriteJob>,
    ) -> Result<()> {
        while let Some(job) = queued.recv().await {
            match writer.write_all(&job.bytes).await {
                Ok(()) => {
                    if let Some(receipt) = job.receipt {
                        let _ = receipt.send(Ok(()));
                    }
                }
                Err(error) => {
                    if let Some(receipt) = job.receipt {
                        let cause = std::io::Error::new(error.kind(), error.to_string());
                        let _ = receipt.send(Err(cause.into()));
                    }
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::time::timeout;

    const DEADLINE: Duration = Duration::from_secs(2);
    const BLOCKED_WRITE_WAIT: Duration = Duration::from_millis(100);

    #[test]
    fn frame_scan_skips_bytes_already_examined() {
        let bytes = b"previous\0next\0";
        assert_eq!(next_frame_end(bytes, 9), Some(13));
        assert_eq!(next_frame_end(bytes, bytes.len()), None);
    }

    #[tokio::test]
    async fn reads_fragmented_and_multiple_frames_without_losing_events() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        peer.write_all(b"{\"method\":\"Test.").await.expect("partial frame");
        tokio::task::yield_now().await;
        peer.write_all(b"first\",\"params\":{}}\0{\"method\":\"Test.second\",\"params\":{}}\0{\"method\":\"Test.third\",\"params\":{").await.expect("two frames and partial tail");
        for method in ["Test.first", "Test.second"] {
            let event = timeout(DEADLINE, client.next())
                .await
                .expect("event deadline")
                .expect("event")
                .expect("event result");
            let value: Value = serde_json::from_str(event.to_text().expect("text")).expect("event JSON");
            assert_eq!(value["method"], method);
        }
        peer.write_all(b"}}\0").await.expect("remaining tail");
        let event = timeout(DEADLINE, client.next())
            .await
            .expect("tail deadline")
            .expect("tail")
            .expect("tail result");
        let value: Value = serde_json::from_str(event.to_text().expect("text")).expect("tail JSON");
        assert_eq!(value["method"], "Test.third");
    }

    fn large_command() -> Message {
        Message::Text(
            serde_json::json!({"id":1,"method":"Runtime.evaluate","params":{"expression":"x".repeat(4 * 1024 * 1024)}})
                .to_string()
                .into(),
        )
    }

    async fn read_command(peer: &mut tokio::net::UnixStream) -> Value {
        timeout(DEADLINE, async {
            let mut bytes = Vec::new();
            loop {
                let byte = peer.read_u8().await.expect("command byte");
                if byte == 0 {
                    return serde_json::from_slice(&bytes).expect("command JSON");
                }
                bytes.push(byte);
            }
        })
        .await
        .expect("command deadline")
    }

    async fn secondary_client(hub: &PipeHub, peer: &mut tokio::net::UnixStream) -> RawConnection {
        let hub = hub.clone();
        let opening = tokio::spawn(async move { hub.open_client().await });
        let command = read_command(peer).await;
        assert_eq!(command["method"], "Target.attachToBrowserTarget");
        let mut response =
            serde_json::to_vec(&serde_json::json!({"id":command["id"],"result":{"sessionId":"secondary"}}))
                .expect("reply JSON");
        response.push(0);
        peer.write_all(&response).await.expect("attach response");
        timeout(DEADLINE, opening)
            .await
            .expect("attach deadline")
            .expect("attach task")
            .expect("secondary client")
    }

    #[tokio::test]
    async fn detached_sessions_stop_routing_events_to_the_previous_client() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, _primary) = PipeHub::start(socket);
        let mut secondary = secondary_client(&hub, &mut peer).await;
        for method in ["Target.attachedToTarget", "Target.detachedFromTarget"] {
            let mut event = serde_json::to_vec(
                &serde_json::json!({"method":method,"sessionId":"secondary","params":{"sessionId":"child"}}),
            )
            .expect("event JSON");
            event.push(0);
            peer.write_all(&event).await.expect("event write");
            let received = timeout(DEADLINE, secondary.next())
                .await
                .expect("event deadline")
                .expect("event")
                .expect("event result");
            let value: Value = serde_json::from_str(received.to_text().expect("text")).expect("event JSON");
            assert_eq!(value["method"], method);
            assert!(value.get("sessionId").is_none());
        }
        peer.write_all(b"{\"method\":\"Test.stale\",\"sessionId\":\"child\",\"params\":{}}\0{\"method\":\"Test.live\",\"sessionId\":\"secondary\",\"params\":{}}\0").await.expect("events write");
        let received = timeout(DEADLINE, secondary.next())
            .await
            .expect("live deadline")
            .expect("live event")
            .expect("live result");
        let value: Value = serde_json::from_str(received.to_text().expect("text")).expect("event JSON");
        assert_eq!(value["method"], "Test.live");
    }

    #[tokio::test]
    async fn closing_a_secondary_client_detaches_without_closing_the_primary() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, mut primary) = PipeHub::start(socket);
        let mut secondary = secondary_client(&hub, &mut peer).await;
        timeout(DEADLINE, secondary.close())
            .await
            .expect("close deadline")
            .expect("close result");
        let command = read_command(&mut peer).await;
        assert_eq!(command["method"], "Target.detachFromTarget");
        assert_eq!(command["params"]["sessionId"], "secondary");
        primary
            .send(Message::Text(
                serde_json::json!({"id":41,"method":"Browser.getVersion","params":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("primary send");
        let command = read_command(&mut peer).await;
        let mut reply = serde_json::to_vec(&serde_json::json!({"id":command["id"],"result":{"product":"TestBrowser"}}))
            .expect("reply JSON");
        reply.push(0);
        peer.write_all(&reply).await.expect("primary response");
        let reply = timeout(DEADLINE, primary.next())
            .await
            .expect("primary deadline")
            .expect("reply")
            .expect("reply result");
        let value: Value = serde_json::from_str(reply.to_text().expect("text")).expect("reply JSON");
        assert_eq!(value["id"], 41);
        assert_eq!(value["result"]["product"], "TestBrowser");
    }

    #[tokio::test]
    async fn flush_waits_until_the_peer_accepts_the_command() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        let command = large_command();
        assert!(timeout(BLOCKED_WRITE_WAIT, client.send(command)).await.is_err());
        let drain = tokio::spawn(async move {
            let mut buffer = [0; READ_BUFFER_SIZE];
            loop {
                let count = peer.read(&mut buffer).await.expect("read command");
                assert_ne!(count, 0);
                if buffer[..count].contains(&0) {
                    return peer;
                }
            }
        });
        timeout(DEADLINE, client.flush())
            .await
            .expect("flush deadline")
            .expect("flush succeeds");
        let _peer = drain.await.expect("drain task");
    }

    #[tokio::test]
    async fn reads_events_while_an_outgoing_command_is_blocked() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, client) = PipeHub::start(socket);
        let (mut outgoing, mut incoming) = client.split();
        let writing = tokio::spawn(async move { outgoing.send(large_command()).await });
        let mut prefix = [0; 1];
        peer.read_exact(&mut prefix).await.expect("write has started");
        peer.write_all(b"{\"method\":\"Test.ready\",\"params\":{}}\0")
            .await
            .expect("event write");
        let event = timeout(DEADLINE, incoming.next())
            .await
            .expect("event deadline")
            .expect("event")
            .expect("event result");
        assert_eq!(
            serde_json::from_str::<Value>(event.to_text().expect("text")).expect("json")["method"],
            "Test.ready"
        );
        drop(peer);
        let _ = writing.await.expect("writer task");
    }

    #[tokio::test]
    async fn dropping_the_last_client_releases_a_blocked_writer() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, mut client) = PipeHub::start(socket);
        assert!(timeout(BLOCKED_WRITE_WAIT, client.send(large_command())).await.is_err());
        drop(client);
        drop(hub);
        let mut received = Vec::new();
        timeout(DEADLINE, peer.read_to_end(&mut received))
            .await
            .expect("pipe EOF deadline")
            .expect("pipe EOF");
        assert!(!received.is_empty());
    }

    #[tokio::test]
    async fn peer_eof_reports_an_error_then_ends_the_stream() {
        let (socket, peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        drop(peer);
        let failure = timeout(DEADLINE, client.next())
            .await
            .expect("EOF deadline")
            .expect("EOF error");
        assert!(matches!(failure, Err(CdpError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof));
        assert!(
            timeout(DEADLINE, client.next())
                .await
                .expect("stream end deadline")
                .is_none()
        );
    }

    #[tokio::test]
    async fn peer_eof_reports_the_failure_to_each_independent_client() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, mut primary) = PipeHub::start(socket);
        let mut secondary = secondary_client(&hub, &mut peer).await;
        drop(peer);
        for client in [&mut primary, &mut secondary] {
            let failure = timeout(DEADLINE, client.next())
                .await
                .expect("EOF deadline")
                .expect("EOF error");
            assert!(matches!(failure, Err(CdpError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof));
            assert!(
                timeout(DEADLINE, client.next())
                    .await
                    .expect("stream end deadline")
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn closing_the_last_client_releases_the_pipe() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, mut client) = PipeHub::start(socket);
        timeout(DEADLINE, client.close())
            .await
            .expect("close deadline")
            .expect("close result");
        drop(hub);
        assert_eq!(
            timeout(DEADLINE, peer.read_u8())
                .await
                .expect("EOF deadline")
                .expect_err("pipe closed")
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn ready_waits_for_the_previous_command_receipt() {
        let (socket, _peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        client.feed(large_command()).await.expect("first command queued");
        assert!(timeout(BLOCKED_WRITE_WAIT, client.feed(large_command())).await.is_err());
    }

    #[tokio::test]
    async fn socket_write_failures_reach_the_command_receipt() {
        struct BrokenWriter;

        impl tokio::io::AsyncWrite for BrokenWriter {
            fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<std::io::Result<usize>> {
                Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed peer")))
            }

            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let (queued, jobs) = mpsc::unbounded_channel();
        let (receipt, done) = oneshot::channel();
        queued
            .send(WriteJob {
                bytes: vec![0],
                receipt: Some(receipt),
            })
            .expect("queue command");
        let failure = Router::write_commands(BrokenWriter, jobs)
            .await
            .expect_err("write fails");
        assert!(matches!(failure, CdpError::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe));
        let failure = done.await.expect("receipt delivered").expect_err("receipt fails");
        assert!(matches!(failure, CdpError::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe));
    }
}
