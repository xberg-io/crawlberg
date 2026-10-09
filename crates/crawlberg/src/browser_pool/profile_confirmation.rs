use std::path::Path;

use chromiumoxide::Browser;
use chromiumoxide::raw_connection::RawConnection;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};

use crate::CrawlError;

pub(super) async fn confirm(browser: &Browser, profile: &Path) -> Result<(), CrawlError> {
    if browser.config().and_then(|config| config.user_data_dir.as_deref()) != Some(profile) {
        return Err(profile_error("Chrome opened a different profile"));
    }
    let directory = tempfile::Builder::new()
        .prefix("crawlberg-confirm-")
        .tempdir_in(profile)
        .map_err(profile_error)?;
    let nonce = directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| profile_error("invalid marker name"))?;
    // ~keep Profile confirmation precedes the main handler driver and the SSRF controller. This private client
    // ~keep drives only a blank page and a local blob; no untrusted page is opened before policy installation.
    let operation = async {
        let mut connection = browser.raw_connection().await.map_err(profile_error)?;
        confirm_namespace(&mut connection, directory.path(), nonce).await
    };
    let result = tokio::time::timeout(super::PROFILE_CONFIRM_TIMEOUT, operation)
        .await
        .map_err(|_| profile_error("Chrome did not use the prepared profile's filesystem namespace"))?;
    result?;
    directory.close().map_err(profile_error)
}

async fn confirm_namespace(connection: &mut RawConnection, directory: &Path, nonce: &str) -> Result<(), CrawlError> {
    let target = command(connection, 0, "Target.createTarget", json!({"url":"about:blank"}), None).await?;
    let target = target["targetId"]
        .as_str()
        .ok_or_else(|| profile_error("missing profile-check target"))?;
    let attached = command(
        connection,
        1,
        "Target.attachToTarget",
        json!({"targetId":target,"flatten":true}),
        None,
    )
    .await?;
    let session = attached["sessionId"]
        .as_str()
        .ok_or_else(|| profile_error("missing profile-check session"))?;
    command(
        connection,
        2,
        "Browser.setDownloadBehavior",
        json!({"behavior":"allow","downloadPath":directory}),
        None,
    )
    .await?;
    let result = download_marker(connection, directory, nonce, session).await;
    let restored = command(
        connection,
        4,
        "Browser.setDownloadBehavior",
        json!({"behavior":"default"}),
        None,
    )
    .await;
    let closed = command(connection, 5, "Target.closeTarget", json!({"targetId":target}), None).await;
    result?;
    restored?;
    closed?;
    Ok(())
}

async fn download_marker(
    connection: &mut RawConnection,
    directory: &Path,
    nonce: &str,
    session: &str,
) -> Result<(), CrawlError> {
    // ~keep A launch-specific blob download proves Chrome shares the prepared profile's filesystem namespace;
    // ~keep launch arguments alone cannot detect a confined snap's private copy of the same path.
    let script = format!(
        "(() => {{ const a = document.createElement('a'); \
         a.href = URL.createObjectURL(new Blob([{}], {{type:'text/plain'}})); \
         a.download = 'profile-proof.txt'; document.body.appendChild(a); a.click(); }})()",
        serde_json::to_string(nonce).map_err(profile_error)?,
    );
    command(
        connection,
        3,
        "Runtime.evaluate",
        json!({"expression":script,"userGesture":true}),
        Some(session),
    )
    .await?;
    let marker = directory.join("profile-proof.txt");
    loop {
        if tokio::fs::read_to_string(&marker)
            .await
            .is_ok_and(|contents| contents == nonce)
        {
            return Ok(());
        }
        tokio::time::sleep(super::PROFILE_USERS_POLL_INTERVAL).await;
    }
}

async fn command(
    connection: &mut RawConnection,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Result<Value, CrawlError> {
    let mut request = json!({"id":id,"method":method,"params":params});
    if let Some(session) = session {
        request["sessionId"] = session.into();
    }
    connection
        .send(async_tungstenite::tungstenite::Message::Text(
            request.to_string().into(),
        ))
        .await
        .map_err(profile_error)?;
    loop {
        let response = connection
            .next()
            .await
            .ok_or_else(|| profile_error("profile-check connection closed"))?
            .map_err(profile_error)?;
        let response: Value =
            serde_json::from_str(response.to_text().map_err(profile_error)?).map_err(profile_error)?;
        if response["id"].as_u64() != Some(id) {
            continue;
        }
        if let Some(error) = response.get("error") {
            return Err(profile_error(error));
        }
        return response
            .get("result")
            .cloned()
            .ok_or_else(|| profile_error("missing profile-check response"));
    }
}

fn profile_error(error: impl std::fmt::Display) -> CrawlError {
    CrawlError::browser_error(format!("failed to confirm Chrome's prepared WebRTC profile: {error}"))
}
