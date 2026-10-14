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
pub fn parse_message<M: serde::de::DeserializeOwned>(text: &str) -> serde_json::Result<M> {
    serde_json::from_str(text).or_else(|initial_error| {
        let repaired = replace_unpaired_surrogate_escapes(text);
        let text = repaired.as_deref().unwrap_or(text);
        let error = if repaired.is_some() {
            match serde_json::from_str(text) {
                Ok(message) => return Ok(message),
                Err(error) => error,
            }
        } else {
            initial_error
        };
        match compatible_network_policy(text) {
            Some(message) => serde_json::from_value(message),
            None => Err(error),
        }
    })
}

// ~keep Chrome 155 renamed this field with the same five policy values; the CDP 0.9.1 DTO
// still requires its legacy name. Alias only failed request-extra-info messages, never invent a policy.
fn compatible_network_policy(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<DuplicateGuard>(text).ok()?;
    let mut message: serde_json::Value = serde_json::from_str(text).ok()?;
    if message.get("method")?.as_str()? != "Network.requestWillBeSentExtraInfo" {
        return None;
    }
    let state = message
        .get_mut("params")?
        .get_mut("clientSecurityState")?
        .as_object_mut()?;
    if state.contains_key("privateNetworkRequestPolicy") {
        return None;
    }
    let policy = state.remove("localNetworkAccessRequestPolicy")?;
    state.insert("privateNetworkRequestPolicy".to_owned(), policy);
    Some(message)
}

// ~keep Value keeps the last duplicate key. Validate before aliasing so a typed refusal
// cannot become acceptance after request or security metadata has been silently overwritten.
struct DuplicateGuard;

impl<'de> serde::Deserialize<'de> for DuplicateGuard {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(Self)
    }
}

impl<'de> serde::de::Visitor<'de> for DuplicateGuard {
    type Value = Self;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self, A::Error> {
        let mut keys = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<Self>()?;
        }
        Ok(self)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(self)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self, E> {
        Ok(self)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self, E> {
        Ok(self)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self, E> {
        Ok(self)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self, E> {
        Ok(self)
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self, E> {
        Ok(self)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self, E> {
        Ok(self)
    }
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

    /// An escaped backslash before `ud83d` is six characters of text. The repair of an unpaired
    /// half in the same message does not change that text.
    #[test]
    fn an_escaped_backslash_is_kept_in_a_repaired_message() {
        let parsed: String = parse_message(r#""lit \\ud83d real \ud83d""#).expect("the message must parse");
        assert_eq!(parsed, "lit \\ud83d real \u{FFFD}");
    }

    #[test]
    fn an_unpaired_half_in_an_object_key_is_read_as_the_replacement_character() {
        let text = r#"{"id":7,"result":{"k\ud83d":"v","\uDE00":1}}"#;
        let reply: serde_json::Value = parse_message(text).expect("the reply must parse");
        assert_eq!(
            reply,
            serde_json::json!({ "id": 7, "result": { "k\u{FFFD}": "v", "\u{FFFD}": 1 } })
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
                    (
                        format!("{}{}", escape(high, upper), escape(low, !upper)),
                        vec![high, low],
                    )
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
                assert_eq!(
                    replace_unpaired_surrogate_escapes(&message),
                    None,
                    "case {case}: {message}"
                );
                unchanged += 1;
            } else {
                assert!(before.is_err(), "case {case}: serde_json must refuse {message}");
                repaired += 1;
            }
        }
        assert!(
            unchanged > 2_000 && repaired > 2_000,
            "{unchanged} unchanged, {repaired} repaired"
        );
    }

    fn security_event(field: &str, policy: &str) -> serde_json::Value {
        serde_json::json!({
            "method": "Network.requestWillBeSentExtraInfo",
            "sessionId": "probe-session",
            "params": {
                "requestId": "probe-request", "associatedCookies": [],
                "headers": {"X-Probe": "value"}, "connectTiming": {"requestTime": 1.25},
                "clientSecurityState": {
                    "initiatorIsSecureContext": true, "initiatorIPAddressSpace": "Local", field: policy
                },
                "siteHasCookieInOtherPartition": true, "appliedNetworkConditionsId": "probe-conditions"
            }
        })
    }

    fn policy_cases() -> [(
        &'static str,
        chromiumoxide_cdp::cdp::browser_protocol::network::PrivateNetworkRequestPolicy,
    ); 5] {
        use chromiumoxide_cdp::cdp::browser_protocol::network::PrivateNetworkRequestPolicy as Policy;
        [
            ("Allow", Policy::Allow),
            ("BlockFromInsecureToMorePrivate", Policy::BlockFromInsecureToMorePrivate),
            ("WarnFromInsecureToMorePrivate", Policy::WarnFromInsecureToMorePrivate),
            ("PermissionBlock", Policy::PermissionBlock),
            ("PermissionWarn", Policy::PermissionWarn),
        ]
    }

    fn assert_security_event(
        text: &str,
        policy: chromiumoxide_cdp::cdp::browser_protocol::network::PrivateNetworkRequestPolicy,
        header: &str,
    ) {
        use chromiumoxide_cdp::cdp::browser_protocol::network::{ClientSecurityState, IpAddressSpace, RequestId};
        use chromiumoxide_cdp::cdp::events::{CdpEvent, CdpEventMessage};
        use chromiumoxide_types::Message;
        let Message::Event(event) = parse_message::<Message<CdpEventMessage>>(text).expect("typed CDP event") else {
            panic!("expected an event");
        };
        assert_eq!(event.method, "Network.requestWillBeSentExtraInfo");
        assert_eq!(event.session_id.as_deref(), Some("probe-session"));
        let CdpEvent::NetworkRequestWillBeSentExtraInfo(event) = event.params else {
            panic!("expected request extra info");
        };
        assert_eq!(event.request_id, RequestId::from("probe-request".to_owned()));
        assert_eq!(event.associated_cookies, Vec::new());
        assert_eq!(
            serde_json::to_value(event.headers).expect("headers"),
            serde_json::json!({"X-Probe": header})
        );
        assert_eq!(event.connect_timing.request_time, 1.25);
        assert_eq!(
            event.client_security_state,
            Some(ClientSecurityState::new(true, IpAddressSpace::Local, policy))
        );
        assert_eq!(event.site_has_cookie_in_other_partition, Some(true));
        assert_eq!(event.applied_network_conditions_id.as_deref(), Some("probe-conditions"));
    }

    #[test]
    fn should_preserve_all_five_modern_network_policy_values_and_event_metadata() {
        for (wire, policy) in policy_cases() {
            let text = security_event("localNetworkAccessRequestPolicy", wire).to_string();
            assert_security_event(&text, policy, "value");
        }
    }

    #[test]
    fn should_preserve_all_five_legacy_network_policy_values() {
        for (wire, policy) in policy_cases() {
            let text = security_event("privateNetworkRequestPolicy", wire).to_string();
            assert_security_event(&text, policy, "value");
        }
    }

    #[test]
    fn should_keep_legacy_policy_when_both_names_are_present() {
        use chromiumoxide_cdp::cdp::browser_protocol::network::PrivateNetworkRequestPolicy;
        let mut event = security_event("privateNetworkRequestPolicy", "Allow");
        event["params"]["clientSecurityState"]["localNetworkAccessRequestPolicy"] =
            serde_json::json!("PermissionBlock");
        assert_security_event(&event.to_string(), PrivateNetworkRequestPolicy::Allow, "value");
    }

    #[test]
    fn should_reject_unknown_or_missing_network_policy_without_inventing_a_value() {
        use chromiumoxide_cdp::cdp::events::CdpEventMessage;
        use chromiumoxide_types::Message;
        let mut missing = security_event("localNetworkAccessRequestPolicy", "Allow");
        missing["params"]["clientSecurityState"]
            .as_object_mut()
            .expect("security state")
            .remove("localNetworkAccessRequestPolicy");
        let mut invalid_legacy = security_event("privateNetworkRequestPolicy", "UnknownPolicy");
        invalid_legacy["params"]["clientSecurityState"]["localNetworkAccessRequestPolicy"] = serde_json::json!("Allow");
        for event in [
            security_event("localNetworkAccessRequestPolicy", "UnknownPolicy"),
            missing,
            invalid_legacy,
        ] {
            parse_message::<Message<CdpEventMessage>>(&event.to_string())
                .expect_err("invalid policy must stay an error");
        }
    }

    #[test]
    fn should_leave_successful_messages_and_unrelated_security_objects_unchanged() {
        use chromiumoxide_cdp::cdp::browser_protocol::network::ClientSecurityState;
        use chromiumoxide_cdp::cdp::events::{CdpEvent, CdpEventMessage};
        use chromiumoxide_types::Message;
        let mut event = security_event("localNetworkAccessRequestPolicy", "PermissionWarn");
        let parsed: serde_json::Value = parse_message(&event.to_string()).expect("untyped JSON");
        assert_eq!(parsed, event);
        parse_message::<ClientSecurityState>(&event["params"]["clientSecurityState"].to_string())
            .expect_err("standalone state is outside the event compatibility path");
        event["method"] = serde_json::json!("Other.example");
        let Message::Event(parsed) =
            parse_message::<Message<CdpEventMessage>>(&event.to_string()).expect("unknown event")
        else {
            panic!("expected an event");
        };
        assert_eq!(parsed.method, "Other.example");
        assert_eq!(parsed.params, CdpEvent::Other(event["params"].clone()));
    }

    #[test]
    fn should_not_alias_a_failed_event_with_a_different_method() {
        use chromiumoxide_cdp::cdp::browser_protocol::network::ClientSecurityState;
        #[derive(Debug, serde::Deserialize)]
        struct OtherParams {
            #[serde(rename = "clientSecurityState")]
            _state: ClientSecurityState,
        }
        #[derive(Debug, serde::Deserialize)]
        struct OtherEvent {
            #[serde(rename = "params")]
            _params: OtherParams,
        }
        let mut event = security_event("localNetworkAccessRequestPolicy", "Allow");
        event["method"] = serde_json::json!("Other.example");
        parse_message::<OtherEvent>(&event.to_string()).expect_err("another method must retain its parse failure");
    }

    fn duplicated_event_fields(field: &str) -> Vec<String> {
        let event = security_event(field, "Allow");
        let text = event.to_string();
        let state = event["params"]["clientSecurityState"].to_string();
        vec![
            text.replace(
                r#""sessionId":"probe-session""#,
                r#""sessionId":"first","sessionId":"probe-session""#,
            ),
            text.replace(
                r#""requestId":"probe-request""#,
                r#""requestId":"first","requestId":"probe-request""#,
            ),
            text.replace(
                r#""requestId":"probe-request""#,
                r#""requestId":"first","request\u0049d":"probe-request""#,
            ),
            text.replace(
                &format!(r#""clientSecurityState":{state}"#),
                &format!(r#""clientSecurityState":{state},"clientSecurityState":{state}"#),
            ),
            text.replace(
                r#""initiatorIsSecureContext":true"#,
                r#""initiatorIsSecureContext":false,"initiatorIsSecureContext":true"#,
            ),
            text.replace(
                &format!(r#""{field}":"Allow""#),
                &format!(r#""{field}":"PermissionBlock","{field}":"Allow""#),
            ),
        ]
    }

    #[test]
    fn should_keep_known_duplicate_fields_rejected_when_aliasing_modern_policy() {
        use chromiumoxide_cdp::cdp::events::CdpEventMessage;
        use chromiumoxide_types::Message;
        let modern = duplicated_event_fields("localNetworkAccessRequestPolicy");
        let legacy = duplicated_event_fields("privateNetworkRequestPolicy");
        assert_eq!(modern.len(), 6);
        assert_eq!(legacy.len(), 6);
        for (case, (modern, legacy)) in modern.iter().zip(&legacy).enumerate() {
            let error = serde_json::from_str::<CdpEventMessage>(legacy).expect_err("legacy duplicate must fail");
            assert!(error.to_string().contains("duplicate field"), "case {case}: {error}");
            parse_message::<Message<CdpEventMessage>>(legacy).expect_err("legacy duplicate must remain refused");
            let original =
                serde_json::from_str::<Message<CdpEventMessage>>(modern).expect_err("unmodified modern message fails");
            let refused = parse_message::<Message<CdpEventMessage>>(modern)
                .expect_err("compatibility must not collapse duplicate fields");
            assert_eq!(
                refused.to_string(),
                original.to_string(),
                "case {case}: keep the original error"
            );
        }
    }

    #[test]
    fn should_reject_nested_metadata_duplicates_in_policy_compatibility() {
        use chromiumoxide_cdp::cdp::events::CdpEventMessage;
        use chromiumoxide_types::Message;
        let text = security_event("localNetworkAccessRequestPolicy", "Allow").to_string();
        let cases = [
            r#""headers":{"X-Probe":"first","X-Probe":"value"}"#,
            r#""headers":{"X-Meta":{"key":1,"key":2},"X-Probe":"value"}"#,
            r#""headers":{"X-Meta":[{"key":1,"k\u0065y":2}],"X-Probe":"value"}"#,
        ];
        for (case, headers) in cases.into_iter().enumerate() {
            let text = text.replace(r#""headers":{"X-Probe":"value"}"#, headers);
            serde_json::from_str::<Message<CdpEventMessage>>(&text).expect_err("modern field needs compatibility");
            assert!(
                parse_message::<Message<CdpEventMessage>>(&text).is_err(),
                "case {case}: compatibility must reject duplicate metadata"
            );
        }
    }

    #[test]
    fn should_preserve_every_json_atom_in_duplicate_free_metadata() {
        use chromiumoxide_cdp::cdp::events::{CdpEvent, CdpEventMessage};
        use chromiumoxide_types::Message;
        let mut event = security_event("localNetworkAccessRequestPolicy", "Allow");
        let headers = serde_json::json!({"X-Meta": [null, true, false, -1, u64::MAX, 0.5, "text", {"child": []}]});
        event["params"]["headers"] = headers.clone();
        let Message::Event(parsed) =
            parse_message::<Message<CdpEventMessage>>(&event.to_string()).expect("valid atoms")
        else {
            panic!("expected an event");
        };
        let CdpEvent::NetworkRequestWillBeSentExtraInfo(parsed) = parsed.params else {
            panic!("expected extra info");
        };
        assert_eq!(serde_json::to_value(parsed.headers).expect("headers"), headers);
    }

    #[test]
    fn should_apply_surrogate_repair_before_modern_network_policy_compatibility() {
        use chromiumoxide_cdp::cdp::browser_protocol::network::PrivateNetworkRequestPolicy;
        let text = security_event("localNetworkAccessRequestPolicy", "PermissionBlock")
            .to_string()
            .replace(r#""value""#, r#""\ud83d""#);
        assert_security_event(&text, PrivateNetworkRequestPolicy::PermissionBlock, "\u{fffd}");
    }
}
