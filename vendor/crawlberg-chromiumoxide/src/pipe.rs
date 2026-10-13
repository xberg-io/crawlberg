//! The private debugging pipe of a launched browser: the hub that owns the pipe, the client of
//! the hub, and the connection that the handler drives over a client. A browser with a
//! debugging port uses nothing of this module.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::ops::Range;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use async_tungstenite::tungstenite::Message;
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_types::{CallId, EventMessage, MethodCall, MethodId};
use futures::{Sink, Stream};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::error::{CdpError, Result};

const READ_BUFFER_SIZE: usize = 16 * 1024;

fn next_frame_end(bytes: &[u8], scanned: usize) -> Option<usize> {
    bytes[scanned..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|end| scanned + end)
}

/// One member of the top-level object of a frame, as byte ranges of the frame: the key's text,
/// the value, and the bytes to cut to remove the member, its comma included.
#[derive(Debug, PartialEq, Eq)]
struct Member {
    key: Range<usize>,
    value: Range<usize>,
    cut: Range<usize>,
}

/// ~keep The hub routes a frame by its top-level members and forwards its bytes unchanged. It
/// ~keep does not parse the frame: a parse to a value and back for every frame cost the hub's
/// ~keep thread 41 percent of its time under a flood of paused requests, Chrome's backlog ahead
/// ~keep of each read grew without bound, and every read of the page waited behind it until
/// ~keep its timeout. The scanner walks the members of the top-level object and skips a nested
/// ~keep value by its brackets and strings. It routes; the connection's parse validates.
///
/// Gives each member to `visit` in its order. Returns `false` when `frame` is not one JSON
/// object, such as a frame that is not JSON at all.
fn scan_members(frame: &[u8], mut visit: impl FnMut(Member)) -> bool {
    let mut at = skip_space(frame, 0);
    if frame.get(at) != Some(&b'{') {
        return false;
    }
    at = skip_space(frame, at + 1);
    if frame.get(at) == Some(&b'}') {
        return skip_space(frame, at + 1) == frame.len();
    }
    let mut previous_end = None;
    loop {
        let key_start = at;
        let Some(key_end) = skip_string(frame, at) else {
            return false;
        };
        at = skip_space(frame, key_end);
        if frame.get(at) != Some(&b':') {
            return false;
        }
        at = skip_space(frame, at + 1);
        let Some(value_end) = skip_value(frame, at) else {
            return false;
        };
        let value = at..value_end;
        at = skip_space(frame, value_end);
        let last = match frame.get(at) {
            Some(b',') => false,
            Some(b'}') => true,
            _ => return false,
        };
        at = skip_space(frame, at + 1);
        let cut = match previous_end {
            Some(previous_end) => previous_end..value_end,
            None if last => key_start..value_end,
            None => key_start..at,
        };
        visit(Member {
            key: key_start + 1..key_end - 1,
            value,
            cut,
        });
        if last {
            return at == frame.len();
        }
        previous_end = Some(value_end);
    }
}

fn skip_space(frame: &[u8], mut at: usize) -> usize {
    while frame.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// The index after the string that opens at `at`, or `None` when it does not open or end.
fn skip_string(frame: &[u8], at: usize) -> Option<usize> {
    if frame.get(at) != Some(&b'"') {
        return None;
    }
    let mut at = at + 1;
    loop {
        match frame.get(at)? {
            b'\\' => at += 2,
            b'"' => return Some(at + 1),
            _ => at += 1,
        }
    }
}

/// The index after the value that starts at `at`: a string, an object or array by its brackets,
/// or a scalar up to the next separator.
fn skip_value(frame: &[u8], mut at: usize) -> Option<usize> {
    match frame.get(at)? {
        b'"' => skip_string(frame, at),
        b'{' | b'[' => {
            let mut depth = 0usize;
            loop {
                match frame.get(at)? {
                    b'"' => {
                        at = skip_string(frame, at)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(at + 1);
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
        }
        _ => {
            let end = frame[at..]
                .iter()
                .position(|byte| matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace())
                .map_or(frame.len(), |length| at + length);
            (end > at).then_some(end)
        }
    }
}

/// The text of the string value in `value`, without its quotes and as it is written: a session
/// id is compared as Chrome writes it, never unescaped.
fn string_text(frame: &[u8], value: &Range<usize>) -> Option<Range<usize>> {
    (value.len() >= 2 && frame[value.start] == b'"').then(|| value.start + 1..value.end - 1)
}

/// The text of the string member `key` of the object in `object`.
fn member_string(frame: &[u8], object: Range<usize>, key: &[u8]) -> Option<Range<usize>> {
    let mut found = None;
    scan_members(&frame[object.clone()], |member| {
        if &frame[object.start + member.key.start..object.start + member.key.end] == key {
            found = string_text(
                frame,
                &(object.start + member.value.start..object.start + member.value.end),
            );
        }
    });
    found
}

fn integer(frame: &[u8], value: &Range<usize>) -> Option<u64> {
    std::str::from_utf8(&frame[value.clone()]).ok()?.parse().ok()
}

/// `frame` with each edit's range replaced by its text. Edits do not overlap.
fn rewritten(frame: &str, mut edits: [Option<(Range<usize>, &str)>; 2]) -> String {
    if let [Some(first), Some(second)] = &edits
        && second.0.start < first.0.start
    {
        edits.swap(0, 1);
    }
    let mut text = String::with_capacity(frame.len() + 20);
    let mut copied = 0;
    for (range, replacement) in edits.into_iter().flatten() {
        text.push_str(&frame[copied..range.start]);
        text.push_str(replacement);
        copied = range.end;
    }
    text.push_str(&frame[copied..]);
    text
}

#[derive(Debug, Clone)]
pub(crate) struct PipeHub(mpsc::UnboundedSender<Command>);

#[derive(Debug)]
enum Command {
    Send(usize, Value),
    Open(oneshot::Sender<Result<PipeClient>>),
    Close(usize),
}

struct WriterTask(tokio::task::JoinHandle<Result<()>>);

impl Drop for WriterTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One CDP client of a browser on a private debugging pipe. It sends and receives the text
/// messages of the protocol, as a WebSocket client of a browser with a debugging port does.
#[derive(Debug)]
pub struct PipeClient {
    id: usize,
    hub: PipeHub,
    incoming: mpsc::UnboundedReceiver<Result<Message>>,
    closing: bool,
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        if !self.closing {
            let _ = self.hub.0.send(Command::Close(self.id));
        }
    }
}

impl PipeClient {
    fn queue_close(&mut self) -> Result<()> {
        self.hub
            .0
            .send(Command::Close(self.id))
            .map_err(|_| CdpError::NoResponse)?;
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

/// ~keep The sink never waits for the hub. A command is queued for the writer task and the sink is
/// ~keep ready for the next one at once, as a WebSocket is while its send buffer has room. A sink
/// ~keep that waited for each write let the connection hand over one command for each wake of its
/// ~keep task, so a page that needed an answer to each of its requests held every other command
/// ~keep back. A failed write ends the hub, and each client reads that error from its stream.
impl Sink<Message> for PipeClient {
    type Error = CdpError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(if self.hub.0.is_closed() || self.closing {
            Err(CdpError::NoResponse)
        } else {
            Ok(())
        })
    }

    fn start_send(self: Pin<&mut Self>, message: Message) -> Result<()> {
        let client = self.get_mut();
        if client.closing {
            return Err(CdpError::NoResponse);
        }
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("id").is_none() {
                    return Err(CdpError::NoResponse);
                }
                client
                    .hub
                    .0
                    .send(Command::Send(client.id, value))
                    .map_err(|_| CdpError::NoResponse)
            }
            Message::Close(_) => client.queue_close(),
            other => Err(CdpError::UnexpectedWsMessage(other)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<()>> {
        let client = self.get_mut();
        if !client.closing {
            client.queue_close()?;
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
        /// The client's id as JSON text, spliced back into the response.
        original_id: String,
    },
    Open {
        done: oneshot::Sender<Result<PipeClient>>,
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
    pub(crate) fn start(socket: tokio::net::UnixStream) -> (Self, PipeClient) {
        let (reader, writer) = socket.into_split();
        Self::start_on(reader, writer)
    }

    /// The hub on any reader and writer. A test gives it a stream whose buffer it knows, or a
    /// writer that fails, so that the test does not depend on the socket buffer of the host.
    fn start_on(
        reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) -> (Self, PipeClient) {
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
            if let Err(error) = router.run(reader, writer, receiver).await {
                tracing::debug!(%error, "private browser pipe closed");
            }
        });
        (hub, client)
    }

    pub(crate) async fn open_client(&self) -> Result<PipeClient> {
        let (sender, receiver) = oneshot::channel();
        self.0.send(Command::Open(sender)).map_err(|_| CdpError::NoResponse)?;
        receiver.await.map_err(|_| CdpError::NoResponse)?
    }
}

impl Router {
    fn add_client(&mut self, root_session: Option<String>) -> PipeClient {
        let id = self.next_client;
        self.next_client += 1;
        let (incoming, receiver) = mpsc::unbounded_channel();
        if let Some(session) = &root_session {
            self.sessions.insert(session.clone(), id);
        }
        self.clients.insert(id, ClientState { root_session, incoming });
        PipeClient {
            id,
            hub: self.hub.clone(),
            incoming: receiver,
            closing: false,
        }
    }

    /// The bytes to write for `command`, with the frame end. `None` for a command that writes nothing.
    fn encode_command(&mut self, command: Command) -> Result<Option<Vec<u8>>> {
        let (mut value, pending) = match command {
            Command::Send(client, mut value) => {
                let state = self.clients.get(&client).ok_or(CdpError::NoResponse)?;
                if value.get("sessionId").is_none()
                    && let Some(root) = &state.root_session
                {
                    value["sessionId"] = Value::String(root.clone());
                }
                let original_id = value.get("id").map(Value::to_string).ok_or(CdpError::NoResponse)?;
                (value, Pending::Reply { client, original_id })
            }
            Command::Open(done) => (
                serde_json::json!({"method":"Target.attachToBrowserTarget","params":{}}),
                Pending::Open { done },
            ),
            Command::Close(client) => {
                let state = self.clients.remove(&client);
                self.sessions.retain(|_, owner| *owner != client);
                let Some(root) = state.and_then(|state| state.root_session) else {
                    return Ok(None);
                };
                (
                    serde_json::json!({"method":"Target.detachFromTarget","params":{"sessionId":root}}),
                    Pending::Ignore,
                )
            }
        };
        let id = self.next_call;
        self.next_call += 1;
        value["id"] = Value::from(id);
        self.pending.insert(id, pending);
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(0);
        Ok(Some(bytes))
    }

    /// Routes one frame, as Chrome wrote it, to the client that owns it. The client gets the
    /// frame's bytes unchanged, except a response's id, which gets the client's id back, and a
    /// secondary client's root session id, which that client never sees.
    fn deliver(&mut self, frame: &str) {
        let bytes = frame.as_bytes();
        let mut id = None;
        let mut session = None;
        let mut method = None;
        let mut params = None;
        let mut result = None;
        let scanned = scan_members(bytes, |member| match &bytes[member.key.clone()] {
            b"id" => id = integer(bytes, &member.value).map(|id| (id, member.value)),
            b"sessionId" => session = string_text(bytes, &member.value).map(|text| (text, member.cut)),
            b"method" => method = string_text(bytes, &member.value),
            b"params" => params = Some(member.value),
            b"result" => result = Some(member.value),
            _ => {}
        });
        if !scanned {
            tracing::debug!(target: "chromiumoxide::conn::raw_ws::parse_errors", frame, "skipped a private browser pipe frame that is not a JSON object");
            return;
        }
        if let Some((id, id_value)) = id {
            match self.pending.remove(&id) {
                Some(Pending::Reply { client, original_id }) => {
                    self.send(client, frame, Some((id_value, original_id.as_str())), session);
                }
                Some(Pending::Open { done }) => {
                    let result = match result.and_then(|result| member_string(bytes, result, b"sessionId")) {
                        Some(session) => Ok(self.add_client(Some(frame[session].to_owned()))),
                        None => Err(CdpError::msg(format!(
                            "failed to attach a private browser client: {frame}"
                        ))),
                    };
                    let _ = done.send(result);
                }
                Some(Pending::Ignore) | None => {}
            }
            return;
        }
        let client = match &session {
            Some((text, _)) => match self.sessions.get(&frame[text.clone()]) {
                Some(client) => *client,
                None => return,
            },
            None => 0,
        };
        let method = method.map(|text| &frame[text]);
        let child = || {
            params
                .clone()
                .and_then(|params| member_string(bytes, params, b"sessionId"))
        };
        if method == Some("Target.attachedToTarget")
            && let Some(child) = child()
        {
            self.sessions.insert(frame[child].to_owned(), client);
        }
        if method == Some("Target.detachedFromTarget")
            && let Some(child) = child()
        {
            self.sessions.remove(&frame[child]);
        }
        self.send(client, frame, None, session);
    }

    /// Queues `frame` for `client` with `id` spliced in, and without the `session` member when
    /// it is the client's root session.
    fn send(
        &self,
        client: usize,
        frame: &str,
        id: Option<(Range<usize>, &str)>,
        session: Option<(Range<usize>, Range<usize>)>,
    ) {
        let Some(state) = self.clients.get(&client) else {
            return;
        };
        let root = session
            .filter(|(text, _)| state.root_session.as_deref() == Some(&frame[text.clone()]))
            .map(|(_, cut)| (cut, ""));
        let text = match (&id, &root) {
            (None, None) => frame.to_owned(),
            _ => rewritten(frame, [id, root]),
        };
        let _ = state.incoming.send(Ok(Message::Text(text.into())));
    }

    async fn run(
        &mut self,
        reader: impl tokio::io::AsyncRead + Unpin,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
        commands: mpsc::UnboundedReceiver<Command>,
    ) -> Result<()> {
        let result = self.run_inner(reader, writer, commands).await;
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
        mut reader: impl tokio::io::AsyncRead + Unpin,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) -> Result<()> {
        let (writes, queued) = mpsc::unbounded_channel();
        let mut writer = WriterTask(tokio::spawn(Self::write_commands(writer, queued)));
        let mut pending_bytes = Vec::with_capacity(READ_BUFFER_SIZE);
        let mut scanned = 0;
        loop {
            pending_bytes.reserve(READ_BUFFER_SIZE);
            tokio::select! {
                command = commands.recv() => {
                    // ~keep Every command that is queued goes to the writer in this turn, as one
                    // ~keep batch. A poll of a task has a budget of 128 operations, and each turn
                    // ~keep of this loop spends three, so a hub that took one command for each
                    // ~keep turn handed over about 40 commands for each time the scheduler ran it.
                    // ~keep Under a flood of paused requests, each of which needs an answer, the
                    // ~keep answers left for Chrome at a few hundred a second and every read of the
                    // ~keep page waited behind them until its timeout.
                    let mut ended = command.is_none();
                    let mut next = command;
                    let mut batch = Vec::new();
                    while let Some(command) = next {
                        if let Some(bytes) = self.encode_command(command)? {
                            batch.extend_from_slice(&bytes);
                        }
                        next = match commands.try_recv() {
                            Ok(command) => Some(command),
                            Err(mpsc::error::TryRecvError::Empty) => None,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                ended = true;
                                None
                            }
                        };
                    }
                    if !batch.is_empty() {
                        writes.send(batch).map_err(|_| CdpError::NoResponse)?;
                    }
                    if ended || self.clients.is_empty() {
                        // ~keep The writer ends when its queue is empty, so a command that a client
                        // ~keep queued before it closed is written before the pipe closes.
                        drop(writes);
                        return (&mut writer.0).await.map_err(|error| CdpError::Io(std::io::Error::other(error)))?;
                    }
                }
                written = &mut writer.0 => {
                    return written.map_err(|error| CdpError::Io(std::io::Error::other(error)))?;
                }
                read = reader.read_buf(&mut pending_bytes) => {
                    if read? == 0 {
                        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "private browser pipe reached EOF").into());
                    }
                    // ~keep Every complete frame in the buffer is delivered in this turn, each
                    // ~keep byte is scanned once, and the bytes of a partial frame move once for
                    // ~keep each read, not once for each frame.
                    let mut consumed = 0;
                    while let Some(end) = next_frame_end(&pending_bytes, scanned) {
                        match std::str::from_utf8(&pending_bytes[consumed..end]) {
                            Ok(frame) => self.deliver(frame),
                            Err(error) => {
                                tracing::debug!(target: "chromiumoxide::conn::raw_ws::parse_errors", %error, "skipped a private browser pipe frame that is not UTF-8");
                            }
                        }
                        consumed = end + 1;
                        scanned = consumed;
                    }
                    pending_bytes.drain(..consumed);
                    scanned = pending_bytes.len();
                }
            }
        }
    }

    async fn write_commands(
        mut writer: impl tokio::io::AsyncWrite + Unpin,
        mut queued: mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> Result<()> {
        while let Some(mut bytes) = queued.recv().await {
            // ~keep Every batch that is queued is written in one call, for the same reason the
            // ~keep hub drains its queue: one write for each turn is one write for each time the
            // ~keep scheduler runs this task.
            while let Ok(more) = queued.try_recv() {
                bytes.extend_from_slice(&more);
            }
            writer.write_all(&bytes).await?;
        }
        Ok(())
    }
}

/// Why the private debugging pipe of a browser failed or closed. Only the connection of the
/// pipe makes one, so [`CdpError::Pipe`] is never the error of a browser with a debugging port.
#[derive(Debug)]
pub struct PipeError(String);

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PipeError {}

fn pipe_error(error: CdpError) -> CdpError {
    CdpError::Pipe(PipeError(error.to_string()))
}

/// The connection of the handler to a browser on a private debugging pipe: the queue of
/// commands and the stream of messages over one [`PipeClient`]. A browser with a debugging
/// port uses [`crate::conn::Connection`], which this type does not share code with.
///
/// ~keep One poll hands every queued command to the client, because the sink of the client is
/// ~keep always ready. A send that fails, or an error that the hub reports, ends the stream
/// ~keep with [`CdpError::Pipe`]: nothing reads the pipe after its hub has ended.
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub(crate) struct PipeConnection<T: EventMessage> {
    pending_commands: VecDeque<MethodCall>,
    client: PipeClient,
    next_id: usize,
    _marker: PhantomData<T>,
}

impl<T: EventMessage> PipeConnection<T> {
    pub(crate) fn new(client: PipeClient) -> Self {
        Self {
            pending_commands: VecDeque::new(),
            client,
            next_id: 0,
            _marker: PhantomData,
        }
    }

    /// Queue the command and return its id, as [`crate::conn::Connection::submit_command`] does.
    pub(crate) fn submit_command(
        &mut self,
        method: MethodId,
        session_id: Option<SessionId>,
        params: Value,
    ) -> serde_json::Result<CallId> {
        let id = CallId::new(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.pending_commands.push_back(MethodCall {
            id,
            method,
            session_id: session_id.map(Into::into),
            params,
        });
        Ok(id)
    }
}

impl<T: EventMessage + Unpin> Stream for PipeConnection<T> {
    type Item = Result<chromiumoxide_types::Message<T>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();
        while let Some(call) = pin.pending_commands.pop_front() {
            tracing::trace!("Sending {:?}", call);
            let text = match serde_json::to_string(&call) {
                Ok(text) => text,
                Err(error) => return Poll::Ready(Some(Err(error.into()))),
            };
            if let Err(error) = Pin::new(&mut pin.client).start_send(Message::Text(text.into())) {
                return Poll::Ready(Some(Err(pipe_error(error))));
            }
        }
        match ready!(Pin::new(&mut pin.client).poll_next(cx)) {
            Some(Ok(Message::Text(text))) => Poll::Ready(Some(
                crate::conn::parse_message::<chromiumoxide_types::Message<T>>(&text).map_err(|error| {
                    tracing::debug!(target: "chromiumoxide::conn::raw_ws::parse_errors", %error, "failed to parse a private browser pipe message");
                    CdpError::InvalidMessage(text.as_str().to_string(), error)
                }),
            )),
            Some(Ok(message)) => Poll::Ready(Some(Err(CdpError::UnexpectedWsMessage(message)))),
            Some(Err(error)) => Poll::Ready(Some(Err(pipe_error(error)))),
            None => Poll::Ready(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromiumoxide_cdp::cdp::CdpEventMessage;
    use futures::{FutureExt, SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::time::timeout;

    /// ~keep A page that floods requests needs one answer for each request, so the commands queue
    /// ~keep up faster than the task of the connection wakes. A WebSocket takes the whole queue in
    /// ~keep one poll. The pipe took one command for each poll, and the read of the document
    /// ~keep waited behind the queue until its timeout (the two flood tests of
    /// ~keep `test_browser_ssrf_whole_fetch`). The test polls the connection one time and never
    /// ~keep again, with no Chrome: the peer is the other end of a socket pair.
    #[tokio::test]
    async fn one_poll_of_the_connection_hands_every_queued_command_to_the_pipe() {
        const COMMANDS: usize = 200;
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_hub, client) = PipeHub::start(socket);
        let mut connection = PipeConnection::<CdpEventMessage>::new(client);
        for number in 0..COMMANDS {
            connection
                .submit_command(
                    "Fetch.failRequest".into(),
                    None,
                    serde_json::json!({"requestId": number.to_string()}),
                )
                .expect("command queued");
        }
        assert!(
            connection.next().now_or_never().is_none(),
            "the peer sent nothing, so the poll has nothing to return"
        );

        let mut frames = 0;
        let all_read = tokio::time::timeout(Duration::from_secs(2), async {
            let mut buffer = [0; 4096];
            while frames < COMMANDS {
                let count = peer.read(&mut buffer).await.expect("read commands");
                assert_ne!(count, 0, "the pipe stays open");
                frames += buffer[..count].iter().filter(|byte| **byte == 0).count();
            }
        })
        .await;
        assert!(
            all_read.is_ok(),
            "one poll must hand all {COMMANDS} commands to the pipe: the peer got {frames}"
        );
    }

    /// The handler ends on [`CdpError::Pipe`] and on no other error of the pipe, so the error
    /// that the hub reports when Chrome closes the pipe must reach the handler as that value.
    #[tokio::test]
    async fn the_end_of_the_pipe_reaches_the_connection_as_a_pipe_error() {
        let (socket, peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_hub, client) = PipeHub::start(socket);
        let mut connection = PipeConnection::<CdpEventMessage>::new(client);
        drop(peer);
        let failure = timeout(DEADLINE, connection.next()).await.expect("failure deadline");
        assert!(
            matches!(failure, Some(Err(CdpError::Pipe(_)))),
            "the closed pipe must be a pipe error: {failure:?}"
        );
    }

    const DEADLINE: Duration = Duration::from_secs(2);

    #[test]
    fn frame_scan_skips_bytes_already_examined() {
        let bytes = b"previous\0next\0";
        assert_eq!(next_frame_end(bytes, 9), Some(13));
        assert_eq!(next_frame_end(bytes, bytes.len()), None);
    }

    /// The members of `frame`, as (key, value, the frame without the member).
    fn members(frame: &str) -> Option<Vec<(String, String, String)>> {
        let mut found = Vec::new();
        scan_members(frame.as_bytes(), |member| {
            found.push((
                frame[member.key].to_owned(),
                frame[member.value].to_owned(),
                format!("{}{}", &frame[..member.cut.start], &frame[member.cut.end..]),
            ))
        })
        .then_some(found)
    }

    #[test]
    fn the_scanner_walks_the_top_level_members_and_skips_nested_values() {
        let frame = r#"{"id":7,"result":{"sessionId":"inner","a":[1,{"b":"}"}],"c":"x,\"sessionId\":\"y\""},"sessionId":"root"}"#;
        assert_eq!(
            members(frame),
            Some(vec![
                ("id".into(), "7".into(), format!("{{{}", &frame[8..])),
                (
                    "result".into(),
                    r#"{"sessionId":"inner","a":[1,{"b":"}"}],"c":"x,\"sessionId\":\"y\""}"#.into(),
                    r#"{"id":7,"sessionId":"root"}"#.into()
                ),
                (
                    "sessionId".into(),
                    "\"root\"".into(),
                    r#"{"id":7,"result":{"sessionId":"inner","a":[1,{"b":"}"}],"c":"x,\"sessionId\":\"y\""}}"#.into()
                ),
            ])
        );
        assert_eq!(
            members(" { \"a\" : true , \"b\" : null } "),
            Some(vec![
                ("a".into(), "true".into(), " { \"b\" : null } ".into()),
                ("b".into(), "null".into(), " { \"a\" : true } ".into()),
            ])
        );
        assert_eq!(members("{}"), Some(Vec::new()));
        assert_eq!(
            members(r#"{"only":"\"\\"}"#),
            Some(vec![("only".into(), r#""\"\\""#.into(), "{}".into())])
        );
        for frame in [
            "not json",
            "[1]",
            "\"x\"",
            "{",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":\"unterminated}",
            "{\"a\":{\"b\":1}",
            "{\"a\":1,}",
            "{\"a\":1} x",
            "{,\"a\":1}",
        ] {
            assert_eq!(members(frame), None, "{frame}");
        }
        let frame = br#"{"params":{"x":{"sessionId":"deep"},"sessionId":"child"}}"#;
        let params = 10..frame.len() - 1;
        assert_eq!(
            member_string(frame, params.clone(), b"sessionId").map(|at| &frame[at]),
            Some(&b"child"[..])
        );
        assert_eq!(member_string(frame, params, b"x"), None);
        assert_eq!(integer(b"{\"id\":12}", &(6..8)), Some(12));
        assert_eq!(integer(b"{\"id\":-1}", &(6..8)), None);
    }

    #[test]
    fn a_rewrite_applies_its_edits_in_order() {
        assert_eq!(
            rewritten(
                "{\"id\":99,\"x\":1,\"sessionId\":\"s\"}",
                [Some((14..30, "")), Some((6..8, "4"))]
            ),
            "{\"id\":4,\"x\":1}"
        );
        assert_eq!(rewritten("{}", [None, None]), "{}");
    }

    /// ~keep A frame reaches its client as Chrome wrote it: a parse to a value and back for every
    /// ~keep frame cost the hub 41 percent of its thread under a flood. A response gets the
    /// ~keep client's id back by a splice, and nothing else changes.
    #[tokio::test]
    async fn a_response_reaches_its_client_byte_for_byte_except_its_id() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        client
            .send(Message::Text(
                "{\"id\":41,\"method\":\"Browser.getVersion\",\"params\":{}}"
                    .to_owned()
                    .into(),
            ))
            .await
            .expect("send");
        let command = read_command(&mut peer).await;
        let response = format!(
            "{{\"id\":{} , \"result\" : {{\"z\":1,\"a\":[1, 2],\"u\":\"\\u00e9\"}},\"extra\":{{}}}}\0",
            command["id"]
        );
        peer.write_all(response.as_bytes()).await.expect("response write");
        let reply = timeout(DEADLINE, client.next())
            .await
            .expect("reply deadline")
            .expect("reply")
            .expect("reply result");
        assert_eq!(
            reply.to_text().expect("text"),
            "{\"id\":41 , \"result\" : {\"z\":1,\"a\":[1, 2],\"u\":\"\\u00e9\"},\"extra\":{}}"
        );
    }

    /// ~keep A secondary client never sees its root session, and nothing else of an event changes:
    /// ~keep not a nested `sessionId`, not the text of a string, not a child session's id.
    #[tokio::test]
    async fn an_event_of_a_secondary_client_loses_only_its_root_session() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, _primary) = PipeHub::start(socket);
        let mut secondary = secondary_client(&hub, &mut peer).await;
        let attached = "{\"method\":\"Target.attachedToTarget\",\"params\":{\"sessionId\":\"child\",\"targetInfo\":{}},\"sessionId\":\"secondary\"}";
        let child = "{\"method\":\"Test.child\",\"params\":{\"k\":\"v,\\\"sessionId\\\":\\\"secondary\\\"\"},\"sessionId\":\"child\"}";
        peer.write_all(format!("{attached}\0{child}\0").as_bytes())
            .await
            .expect("events write");
        for expected in [
            "{\"method\":\"Target.attachedToTarget\",\"params\":{\"sessionId\":\"child\",\"targetInfo\":{}}}",
            child,
        ] {
            let event = timeout(DEADLINE, secondary.next())
                .await
                .expect("event deadline")
                .expect("event")
                .expect("event result");
            assert_eq!(event.to_text().expect("text"), expected);
        }
    }

    /// A frame with half of a surrogate pair reaches the client as Chrome wrote it, and the
    /// connection's parse reads it as the WebSocket connection reads such a message: an event
    /// with U+FFFD in its place. A frame that is not JSON is skipped, and the pipe stays open for
    /// the event after it.
    #[tokio::test]
    async fn a_frame_that_is_not_json_is_skipped_and_the_next_event_arrives() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        let half = "{\"method\":\"Test.half\",\"params\":{\"title\":\"\\ud83d\"}}";
        peer.write_all(format!("{half}\0not json\0{{\"method\":\"Test.next\",\"params\":{{}}}}\0").as_bytes())
            .await
            .expect("a frame with half a pair, a frame that is not JSON and a good frame");
        let event = timeout(DEADLINE, client.next())
            .await
            .expect("half deadline")
            .expect("half event")
            .expect("a frame with half a surrogate pair is an event");
        assert_eq!(
            event.to_text().expect("text"),
            half,
            "the frame arrives as Chrome wrote it"
        );
        let value: Value = crate::conn::parse_message(event.to_text().expect("text")).expect("half event JSON");
        assert_eq!(value["method"], "Test.half");
        assert_eq!(value["params"]["title"], "\u{FFFD}");
        let event = timeout(DEADLINE, client.next())
            .await
            .expect("event deadline")
            .expect("event")
            .expect("the pipe stays open after a frame that is not JSON");
        let value: Value = serde_json::from_str(event.to_text().expect("text")).expect("event JSON");
        assert_eq!(value["method"], "Test.next");
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

    async fn secondary_client(hub: &PipeHub, peer: &mut tokio::net::UnixStream) -> PipeClient {
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

    /// ~keep The commands are larger together than the buffer of the socket, and the peer reads
    /// ~keep nothing while the client sends them. A sink that waits for a write stops at the first.
    #[tokio::test]
    async fn a_client_sends_a_flood_of_commands_while_the_peer_reads_nothing() {
        const COMMANDS: u64 = 500;
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (_, mut client) = PipeHub::start(socket);
        for number in 0..COMMANDS {
            let command = serde_json::json!({
                "id": number,
                "method": "Fetch.failRequest",
                "params": {"number": number, "requestId": "r".repeat(1024)},
            });
            client
                .send(Message::Text(command.to_string().into()))
                .now_or_never()
                .expect("the sink takes a command with no wait for the peer")
                .expect("command queued");
        }
        for number in 0..COMMANDS {
            let mut event =
                serde_json::to_vec(&serde_json::json!({"method":"Fetch.requestPaused","params":{"number":number}}))
                    .expect("event JSON");
            event.push(0);
            peer.write_all(&event).await.expect("event write");
        }
        for number in 0..COMMANDS {
            let event = timeout(DEADLINE, client.next())
                .await
                .expect("event deadline")
                .expect("event")
                .expect("event result");
            let value: Value = serde_json::from_str(event.to_text().expect("text")).expect("event JSON");
            assert_eq!(value["params"]["number"], number, "each event arrives in its order");
        }
        for number in 0..COMMANDS {
            let command = read_command(&mut peer).await;
            assert_eq!(
                command["params"]["number"], number,
                "each command is written in its order"
            );
        }
    }

    /// ~keep A poll of a task has a budget of 128 operations. A hub that took one command for each
    /// ~keep turn and a writer that wrote one command for each turn moved about 40 commands for
    /// ~keep each round of the scheduler. Under a flood that needs an answer for each request the
    /// ~keep answers left at a few hundred a second, and each read of the page waited behind them
    /// ~keep until its timeout (the two flood tests of `test_browser_ssrf_whole_fetch`). The test
    /// ~keep gives the hub and the writer three rounds for a thousand commands. The stream is an
    /// ~keep in-memory one whose buffer holds every command: a socket's buffer depends on the host
    /// ~keep (macOS takes 70 of the 1000 before the peer reads).
    #[tokio::test]
    async fn every_queued_command_is_written_within_three_rounds_of_the_scheduler() {
        const COMMANDS: u64 = 1000;
        let (stream, mut peer) = tokio::io::duplex(4 * 1024 * 1024);
        let (reader, writer) = tokio::io::split(stream);
        let (_, mut client) = PipeHub::start_on(reader, writer);
        for number in 0..COMMANDS {
            let command = serde_json::json!({
                "id": number,
                "method": "Fetch.failRequest",
                "params": {"requestId": "interception-job-1.0", "errorReason": "BlockedByClient"},
            });
            client
                .send(Message::Text(command.to_string().into()))
                .now_or_never()
                .expect("the sink takes a command with no wait for the peer")
                .expect("command queued");
        }
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        let mut received = 0;
        let mut buffer = [0; READ_BUFFER_SIZE];
        while let Some(read) = peer.read(&mut buffer).now_or_never() {
            match read {
                Ok(0) => panic!("the pipe stays open"),
                Ok(count) => received += buffer[..count].iter().filter(|byte| **byte == 0).count() as u64,
                Err(error) => panic!("read commands: {error}"),
            }
        }
        assert_eq!(
            received, COMMANDS,
            "every queued command is written within three rounds of the scheduler"
        );
    }

    /// ~keep The batches that queue while the writer waits on the socket go out in one write.
    #[tokio::test]
    async fn the_writer_writes_every_queued_batch_in_one_call() {
        struct CountingWriter(Vec<usize>);

        impl tokio::io::AsyncWrite for CountingWriter {
            fn poll_write(mut self: Pin<&mut Self>, _: &mut Context<'_>, bytes: &[u8]) -> Poll<std::io::Result<usize>> {
                self.0.push(bytes.len());
                Poll::Ready(Ok(bytes.len()))
            }

            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let (queued, jobs) = mpsc::unbounded_channel();
        for _ in 0..3 {
            queued.send(b"{}\0".to_vec()).expect("batch queued");
        }
        drop(queued);
        let mut writer = CountingWriter(Vec::new());
        Router::write_commands(&mut writer, jobs)
            .await
            .expect("the writer ends with its queue");
        assert_eq!(writer.0, vec![9], "three queued batches are written in one call");
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
    async fn the_command_of_a_dropped_client_is_written_before_the_pipe_closes() {
        let (socket, mut peer) = tokio::net::UnixStream::pair().expect("socket pair");
        let (hub, mut client) = PipeHub::start(socket);
        client
            .send(large_command())
            .now_or_never()
            .expect("the sink takes a command with no wait for the peer")
            .expect("command queued");
        drop(client);
        drop(hub);
        let mut received = Vec::new();
        timeout(DEADLINE, peer.read_to_end(&mut received))
            .await
            .expect("pipe EOF deadline")
            .expect("pipe EOF");
        assert_eq!(received.last(), Some(&0), "the command ends with its frame end");
        let command: Value = serde_json::from_slice(&received[..received.len() - 1]).expect("one whole command");
        assert_eq!(command["method"], "Runtime.evaluate");
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

    /// ~keep The writer fails with BrokenPipe on the real writer task and hub path, while the
    /// ~keep reader stays open: only the write fails. A socket whose peer shut its read side does
    /// ~keep not fail the write on every host (macOS keeps it pending).
    #[tokio::test]
    async fn a_failed_write_reaches_the_client_as_an_error_on_its_stream() {
        struct BrokenWriter;

        impl tokio::io::AsyncWrite for BrokenWriter {
            fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<std::io::Result<usize>> {
                Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the peer is gone",
                )))
            }

            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let (stream, _peer_keeps_the_read_side_open) = tokio::io::duplex(READ_BUFFER_SIZE);
        let (reader, _) = tokio::io::split(stream);
        let (_, mut client) = PipeHub::start_on(reader, BrokenWriter);
        client
            .send(Message::Text(
                "{\"id\":1,\"method\":\"Browser.getVersion\",\"params\":{}}"
                    .to_owned()
                    .into(),
            ))
            .now_or_never()
            .expect("the sink takes a command with no wait for the peer")
            .expect("command queued");
        let failure = timeout(DEADLINE, client.next())
            .await
            .expect("error deadline")
            .expect("error");
        assert!(matches!(failure, Err(CdpError::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe));
    }
}
