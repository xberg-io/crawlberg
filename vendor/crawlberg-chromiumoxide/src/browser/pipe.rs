use futures::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;

use super::{Child, LaunchConnection};
use crate::conn::Connection;
use crate::error::{CdpError, Result};
use crate::pipe::PipeHub;

pub(super) async fn initialize(
    pipe: std::os::unix::net::UnixStream,
    child: &mut Child,
    timeout: std::time::Duration,
) -> Result<LaunchConnection> {
    pipe.set_nonblocking(true)?;
    let (hub, mut connection) = PipeHub::start(tokio::net::UnixStream::from_std(pipe)?);
    // ~keep Chrome still writes diagnostics on stderr in pipe mode; drain it so its buffer cannot block CDP.
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut stderr = stderr.into_inner();
            let mut buffer = [0; 4096];
            while let Ok(count) = stderr.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                tracing::trace!(stderr = %String::from_utf8_lossy(&buffer[..count]), "browser diagnostic");
            }
        });
    }
    let ready = async {
        connection
            .send(async_tungstenite::tungstenite::Message::Text(
                r#"{"id":0,"method":"Browser.getVersion","params":{}}"#.into(),
            ))
            .await?;
        loop {
            let message = connection.next().await.ok_or(CdpError::NoResponse)??;
            if let async_tungstenite::tungstenite::Message::Text(text) = message {
                let response: serde_json::Value = serde_json::from_str(&text)?;
                if response.get("id").and_then(serde_json::Value::as_u64) == Some(0) {
                    if response.get("error").is_some() {
                        return Err(CdpError::msg(response.to_string()));
                    }
                    return Ok(());
                }
            }
        }
    };
    tokio::time::timeout(timeout, ready)
        .await
        .map_err(|_| CdpError::Timeout)??;
    Ok(LaunchConnection {
        address: "pipe".into(),
        connection: Connection::from_raw(connection),
        hub: Some(hub),
    })
}
