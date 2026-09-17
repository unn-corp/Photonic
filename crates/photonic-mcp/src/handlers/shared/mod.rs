pub(crate) mod cloning;
pub(crate) mod ordering;
pub(crate) mod paths;
pub(crate) mod random;
pub(crate) mod spatial;
pub(crate) mod styling;

use std::time::Duration;
use tokio::sync::oneshot;

/// Maximum time an MCP capture handler waits for the GUI render loop.
pub(crate) const CAPTURE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureWaitError {
    Timeout,
    Disconnected,
}

/// Wait for a render-thread capture without allowing a stalled frame loop to
/// keep the MCP request alive indefinitely.
pub(crate) async fn wait_for_capture(
    rx: oneshot::Receiver<Vec<u8>>,
) -> Result<Vec<u8>, CaptureWaitError> {
    match tokio::time::timeout(CAPTURE_RESPONSE_TIMEOUT, rx).await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) => Err(CaptureWaitError::Disconnected),
        Err(_) => Err(CaptureWaitError::Timeout),
    }
}
