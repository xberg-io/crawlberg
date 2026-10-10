use std::collections::VecDeque;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::ready;

use async_tungstenite::tungstenite::Message as WsMessage;
use async_tungstenite::{WebSocketStream, tungstenite::protocol::WebSocketConfig};
use futures::stream::Stream;
use futures::task::{Context, Poll};
use futures::{SinkExt, StreamExt};

use async_tungstenite::tokio::ConnectStream;
use chromiumoxide_cdp::cdp::browser_protocol::target::SessionId;
use chromiumoxide_types::{CallId, EventMessage, Message, MethodCall, MethodId};

use crate::error::CdpError;
use crate::error::Result;

/// Exchanges the messages with the websocket
#[must_use = "streams do nothing unless polled"]
#[derive(Debug)]
pub struct Connection<T: EventMessage> {
    /// Queue of commands to send.
    pending_commands: VecDeque<MethodCall>,
    /// The websocket of the chromium instance
    ws: WebSocketStream<ConnectStream>,
    /// The identifier for a specific command
    next_id: usize,
    needs_flush: bool,
    /// The message that is currently being proceessed
    pending_flush: Option<MethodCall>,
    _marker: PhantomData<T>,
}

impl<T: EventMessage + Unpin> Connection<T> {
    pub async fn connect(debug_ws_url: impl AsRef<str>) -> Result<Self> {
        let config = WebSocketConfig::default().max_message_size(None).max_frame_size(None);

        let (ws, _) = async_tungstenite::tokio::connect_async_with_config(debug_ws_url.as_ref(), Some(config)).await?;

        Ok(Self {
            pending_commands: Default::default(),
            ws,
            next_id: 0,
            needs_flush: false,
            pending_flush: None,
            _marker: Default::default(),
        })
    }
}

impl<T: EventMessage> Connection<T> {
    fn next_call_id(&mut self) -> CallId {
        let id = CallId::new(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Queue in the command to send over the socket and return the id for this
    /// command
    pub fn submit_command(
        &mut self,
        method: MethodId,
        session_id: Option<SessionId>,
        params: serde_json::Value,
    ) -> serde_json::Result<CallId> {
        let id = self.next_call_id();
        let call = MethodCall {
            id,
            method,
            session_id: session_id.map(Into::into),
            params,
        };
        self.pending_commands.push_back(call);
        Ok(id)
    }

    /// flush any processed message and start sending the next over the conn
    /// sink
    fn start_send_next(&mut self, cx: &mut Context<'_>) -> Result<()> {
        if self.needs_flush {
            if let Poll::Ready(Ok(())) = self.ws.poll_flush_unpin(cx) {
                self.needs_flush = false;
            }
        }
        if self.pending_flush.is_none() && !self.needs_flush {
            if let Some(cmd) = self.pending_commands.pop_front() {
                tracing::trace!("Sending {:?}", cmd);
                let msg = serde_json::to_string(&cmd)?;
                self.ws.start_send_unpin(msg.into())?;
                self.pending_flush = Some(cmd);
            }
        }
        Ok(())
    }
}

impl<T: EventMessage + Unpin> Stream for Connection<T> {
    type Item = Result<Message<T>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        loop {
            // queue in the next message if not currently flushing
            if let Err(err) = pin.start_send_next(cx) {
                return Poll::Ready(Some(Err(err)));
            }

            // send the message
            if let Some(call) = pin.pending_flush.take() {
                if pin.ws.poll_ready_unpin(cx).is_ready() {
                    pin.needs_flush = true;
                    // try another flush
                    continue;
                } else {
                    pin.pending_flush = Some(call);
                }
            }

            break;
        }

        // read from the ws
        match ready!(pin.ws.poll_next_unpin(cx)) {
            Some(Ok(WsMessage::Text(text))) => {
                let ready = match parse_message::<Message<T>>(&text) {
                    Ok(msg) => {
                        tracing::trace!("Received {:?}", msg);
                        Ok(msg)
                    }
                    Err(err) => {
                        let msg = text.as_str().to_string();
                        tracing::debug!(target: "chromiumoxide::conn::raw_ws::parse_errors", msg, "Failed to parse raw WS message {}", err);
                        Err(CdpError::InvalidMessage(text.as_str().to_string(), err))
                    }
                };
                Poll::Ready(Some(ready))
            }
            Some(Ok(WsMessage::Close(_))) => Poll::Ready(None),
            // ignore ping and pong
            Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Some(Ok(msg)) => Poll::Ready(Some(Err(CdpError::UnexpectedWsMessage(msg)))),
            Some(Err(err)) => Poll::Ready(Some(Err(CdpError::Ws(err)))),
            None => {
                // ws connection closed
                Poll::Ready(None)
            }
        }
    }
}

/// Parse one protocol message, as the connection parses each message it receives.
///
/// Chrome writes a JavaScript string as the page built it, so one half of a surrogate pair
/// arrives as a lone `\ud83d` escape. `serde_json` refuses that escape in a `String`, so the
/// whole reply or event was lost and the command that waited for it waited until its timeout.
/// Such a message is parsed again with each unpaired half replaced by U+FFFD, which is what
/// [`String::from_utf16_lossy`] makes of it.
///
/// A message that `serde_json` reads is returned as `serde_json` read it: the text is searched
/// and parsed a second time only after a refusal.
fn parse_message<M: serde::de::DeserializeOwned>(text: &str) -> serde_json::Result<M> {
    serde_json::from_str(text).or_else(|error| match replace_unpaired_surrogate_escapes(text) {
        Some(repaired) => serde_json::from_str(&repaired),
        None => Err(error),
    })
}

/// The JSON `text` with every `\u` escape of an unpaired surrogate replaced by `�`, or
/// `None` when it has no such escape.
fn replace_unpaired_surrogate_escapes(text: &str) -> Option<String> {
    const HIGH: std::ops::RangeInclusive<u16> = 0xD800..=0xDBFF;
    const LOW: std::ops::RangeInclusive<u16> = 0xDC00..=0xDFFF;
    const ESCAPE_LEN: usize = 6;

    let bytes = text.as_bytes();
    let mut repaired: Option<String> = None;
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            at += 1;
            continue;
        }
        let Some(unit) = escaped_unit(bytes, at) else {
            // Any other escape is a backslash and one character, an escaped backslash included.
            at += 2;
            continue;
        };
        let end = at + ESCAPE_LEN;
        if HIGH.contains(&unit) && escaped_unit(bytes, end).is_some_and(|next| LOW.contains(&next)) {
            at = end + ESCAPE_LEN;
            continue;
        }
        if HIGH.contains(&unit) || LOW.contains(&unit) {
            let repaired = repaired.get_or_insert_with(|| String::with_capacity(text.len()));
            repaired.push_str(&text[copied..at]);
            repaired.push_str("\\ufffd");
            copied = end;
        }
        at = end;
    }
    let mut repaired = repaired?;
    repaired.push_str(&text[copied..]);
    Some(repaired)
}

/// The UTF-16 code unit of the `\uXXXX` escape that starts at `at`, if one starts there.
fn escaped_unit(bytes: &[u8], at: usize) -> Option<u16> {
    let escape = bytes.get(at..at + 6)?;
    if &escape[..2] != b"\\u" || !escape[2..].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u16::from_str_radix(std::str::from_utf8(&escape[2..]).ok()?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::{parse_message, replace_unpaired_surrogate_escapes};

    /// The string parsed from a JSON string literal with these UTF-16 code units as `\u`
    /// escapes, between an `a` and a `z`.
    fn parsed_units(units: &[u16]) -> String {
        let escapes: String = units.iter().map(|unit| format!("\\u{unit:04x}")).collect();
        parse_message::<String>(&format!("\"a{escapes}z\"")).expect("the message must parse")
    }

    /// Chrome writes half of a surrogate pair in a page's text as a lone escape. It is read as
    /// the replacement character, as `String::from_utf16_lossy` reads it, and so the reply is
    /// kept (xberg-io/crawlberg#631).
    #[test]
    fn an_unpaired_surrogate_escape_is_read_as_the_replacement_character() {
        let cases: [&[u16]; 10] = [
            &[0xD83D],
            &[0xDE00],
            &[0xDE00, 0xD83D],
            &[0xD83D, 0xD83D, 0xDE00],
            &[0xD83D, 0xDE00, 0xDE00],
            &[0xD83D, 0x0041],
            &[0xDBFF],
            &[0xDC00],
            &[0xD800, 0xDFFF, 0xD800],
            &[0xD7FF, 0xDC00, 0xE000],
        ];
        for units in cases {
            let expected = format!("a{}z", String::from_utf16_lossy(units));
            assert!(expected.contains('\u{FFFD}'), "{units:04x?} must hold an unpaired half");
            assert_eq!(parsed_units(units), expected, "{units:04x?}");
        }
    }

    #[test]
    fn a_whole_pair_and_every_other_escape_are_kept() {
        assert_eq!(parsed_units(&[0xD83D, 0xDE00]), "a\u{1F600}z");
        assert_eq!(parsed_units(&[0xD7FF, 0xE000, 0x0041]), "a\u{D7FF}\u{E000}Az");
        for (text, expected) in [
            (
                r#""an escaped backslash \\ud83d is text""#,
                "an escaped backslash \\ud83d is text",
            ),
            (r#""\\\\ud83d""#, "\\\\ud83d"),
            (r#""\"\\\/\b\f\n\r\t""#, "\"\\/\u{8}\u{c}\n\r\t"),
        ] {
            let parsed: String = parse_message(text).expect("valid JSON");
            assert_eq!(parsed, expected, "{text}");
        }
    }

    #[test]
    fn only_the_unpaired_half_of_a_reply_changes() {
        let text = r#"{"id":7,"result":{"value":"é \\ 😀 before \ud83d after \" \uDE00"},"sessionId":"S"}"#;
        let reply: serde_json::Value = parse_message(text).expect("the reply must parse");
        assert_eq!(
            reply,
            serde_json::json!({
                "id": 7,
                "result": { "value": "é \\ \u{1F600} before \u{FFFD} after \" \u{FFFD}" },
                "sessionId": "S",
            })
        );
    }

    #[test]
    fn a_message_that_is_not_json_is_still_refused() {
        for text in [
            "",
            "{",
            r#""\ud83"#,
            r#""\ud83d"#,
            r#"{"id":"\ud83d"#,
            "\\",
            r#""\ud83d\u00""#,
        ] {
            assert!(parse_message::<serde_json::Value>(text).is_err(), "{text}");
        }
        let error = parse_message::<serde_json::Value>("{\"id\":}").expect_err("not JSON");
        assert_eq!(error.to_string(), "expected value at line 1 column 7");
    }

    /// A small deterministic generator (xorshift), so that a failure names its seed.
    struct Generator(u64);

    impl Generator {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }

        fn unit_in(&mut self, first: u16, last: u16) -> u16 {
            first + self.below(usize::from(last - first) + 1) as u16
        }

        /// One piece of the inside of a JSON string literal, and the UTF-16 code units a
        /// reader of JSON gets from it.
        fn piece(&mut self) -> (String, Vec<u16>) {
            let text = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
            let escape = |unit: u16, upper: bool| {
                if upper {
                    format!("\\u{unit:04X}")
                } else {
                    format!("\\u{unit:04x}")
                }
            };
            let upper = self.below(2) == 0;
            match self.below(13) {
                0 => ("q".to_owned(), text("q")),
                1 => ("u".to_owned(), text("u")),
                2 => ("d83d".to_owned(), text("d83d")),
                3 => ("\\\\".to_owned(), text("\\")),
                4 => ("\\\"".to_owned(), text("\"")),
                5 => ("\\n".to_owned(), text("\n")),
                6 => ("\\/".to_owned(), text("/")),
                7 => ("é".to_owned(), text("é")),
                8 => ("😀".to_owned(), text("😀")),
                9 => {
                    let unit = [0x0041, 0x00E9, 0xD7FF, 0xE000, 0xFFFD, 0xFFFF][self.below(6)];
                    (escape(unit, upper), vec![unit])
                }
                10 => {
                    let (high, low) = (self.unit_in(0xD800, 0xDBFF), self.unit_in(0xDC00, 0xDFFF));
                    (format!("{}{}", escape(high, upper), escape(low, !upper)), vec![high, low])
                }
                11 => {
                    let high = self.unit_in(0xD800, 0xDBFF);
                    (escape(high, upper), vec![high])
                }
                _ => {
                    let low = self.unit_in(0xDC00, 0xDFFF);
                    (escape(low, upper), vec![low])
                }
            }
        }
    }

    /// Every message is read as its UTF-16 code units read with `String::from_utf16_lossy`. A
    /// message with no unpaired half is read exactly as `serde_json` alone reads it, and is not
    /// searched into a second text. A message with an unpaired half is one `serde_json` refuses.
    #[test]
    fn a_message_without_an_unpaired_half_parses_as_before() {
        let mut generator = Generator(0x9E37_79B9_7F4A_7C15);
        let (mut unchanged, mut repaired) = (0_u32, 0_u32);
        for case in 0..20_000 {
            let mut literal = String::from("\"");
            let mut units = Vec::new();
            for _ in 0..generator.below(12) {
                let (piece, piece_units) = generator.piece();
                literal.push_str(&piece);
                units.extend(piece_units);
            }
            literal.push('"');
            let message = format!(r#"{{"id":{case},"result":{{"value":{literal}}},"method":{literal}}}"#);
            let expected = String::from_utf16_lossy(&units);

            let parsed: serde_json::Value =
                parse_message(&message).unwrap_or_else(|error| panic!("case {case}: {message}: {error}"));
            assert_eq!(
                parsed,
                serde_json::json!({ "id": case, "result": { "value": expected }, "method": expected }),
                "case {case}: {message}"
            );

            let before = serde_json::from_str::<serde_json::Value>(&message);
            if String::from_utf16(&units).is_ok() {
                assert_eq!(before.ok(), Some(parsed), "case {case}: {message}");
                assert_eq!(replace_unpaired_surrogate_escapes(&message), None, "case {case}: {message}");
                unchanged += 1;
            } else {
                assert!(before.is_err(), "case {case}: serde_json must refuse {message}");
                repaired += 1;
            }
        }
        assert!(unchanged > 2_000 && repaired > 2_000, "{unchanged} unchanged, {repaired} repaired");
    }
}
