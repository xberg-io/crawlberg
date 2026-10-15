use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::CdpError;

#[derive(Debug)]
pub(crate) struct OwnedTermination {
    owner: u32,
    requested: AtomicBool,
    observed: AtomicBool,
}

impl Default for OwnedTermination {
    fn default() -> Self {
        Self {
            owner: std::process::id(),
            requested: AtomicBool::new(false),
            observed: AtomicBool::new(false),
        }
    }
}

impl OwnedTermination {
    pub(crate) fn request(&self) {
        if self.owner == std::process::id() {
            self.requested.store(true, Ordering::Release);
        }
    }

    pub(crate) fn observe(&self, error: Option<&CdpError>) -> bool {
        if self.owner != std::process::id()
            || !self.requested.load(Ordering::Acquire)
            || !error.is_none_or(is_terminal_close)
        {
            return false;
        }
        self.observed.store(true, Ordering::Release);
        true
    }

    pub(crate) fn observed(&self) -> bool {
        self.owner == std::process::id() && self.observed.load(Ordering::Acquire)
    }
}

fn is_terminal_close(error: &CdpError) -> bool {
    use async_tungstenite::tungstenite::{Error, error::ProtocolError};
    match error {
        CdpError::Ws(
            Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
        ) => true,
        CdpError::Ws(Error::Io(error)) | CdpError::Io(error) => terminal_io(error.kind()),
        #[cfg(unix)]
        CdpError::Pipe(error) => error.kind().is_some_and(terminal_io),
        _ => false,
    }
}

pub(crate) fn terminal_io(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_eof_and_owned_nonterminal_errors_are_not_expected() {
        let state = OwnedTermination::default();
        let eof = CdpError::Ws(async_tungstenite::tungstenite::Error::Protocol(
            async_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ));
        assert!(!state.observe(Some(&eof)));
        assert!(!state.observed());
        state.request();
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Other,
            std::io::ErrorKind::TimedOut,
        ] {
            let error = CdpError::Ws(async_tungstenite::tungstenite::Error::Io(std::io::Error::from(kind)));
            assert!(!state.observe(Some(&error)), "{kind:?}");
        }
        assert!(!state.observe(Some(&CdpError::Timeout)));
        assert!(!state.observed());
        assert!(state.observe(Some(&eof)));
        assert!(state.observed());
    }

    #[test]
    fn raw_pipe_io_end_requires_owned_intent_and_preserves_nonterminal_errors() {
        for kind in [
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
        ] {
            let state = OwnedTermination::default();
            let error = CdpError::Io(std::io::Error::from(kind));
            assert!(!state.observe(Some(&error)), "active {kind:?}");
            assert!(!state.observed());
            state.request();
            assert!(state.observe(Some(&error)), "owned {kind:?}");
            assert!(state.observed());
        }
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Other,
            std::io::ErrorKind::TimedOut,
        ] {
            let state = OwnedTermination::default();
            state.request();
            assert!(
                !state.observe(Some(&CdpError::Io(std::io::Error::from(kind)))),
                "{kind:?}"
            );
            assert!(!state.observed());
        }
    }
}
