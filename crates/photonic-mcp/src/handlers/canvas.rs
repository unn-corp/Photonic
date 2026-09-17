use crate::handlers::shared::{wait_for_capture, CaptureWaitError};
use crate::protocol::{ScreenshotArgs, ToolResult};
use crate::server::AppState;
use base64::{engine::general_purpose, Engine};
use tokio::sync::oneshot;

pub async fn screenshot(state: &AppState, args: ScreenshotArgs) -> ToolResult {
    tracing::debug!("tool: screenshot — sending to render thread");
    let (tx, rx) = oneshot::channel::<Vec<u8>>();

    // Send the oneshot sender to the render thread via std::sync::mpsc
    let sent = state
        .capture_tx
        .lock()
        .map(|tx_guard| tx_guard.send(tx).is_ok())
        .unwrap_or(false);

    if !sent {
        return ToolResult::error("Screenshot unavailable — render request channel is closed");
    }

    let png_bytes = match wait_for_capture(rx).await {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => return ToolResult::error("Render thread returned empty screenshot data"),
        Err(CaptureWaitError::Timeout) => {
            tracing::warn!("tool: screenshot — render thread response timed out");
            return ToolResult::error(
                "Screenshot timed out waiting for the render thread to return image data",
            );
        }
        Err(CaptureWaitError::Disconnected) => {
            tracing::warn!("tool: screenshot — render thread closed the response channel");
            return ToolResult::error(
                "Render thread closed the screenshot response channel before returning image data",
            );
        }
    };

    tracing::debug!("tool: screenshot — received {} bytes", png_bytes.len());

    // Downscale if requested (reduces base64 size significantly)
    let final_bytes = if let Some(scale) = args.scale {
        if scale > 0.0 && scale < 1.0 {
            downscale_png(&png_bytes, scale).unwrap_or(png_bytes)
        } else {
            png_bytes
        }
    } else {
        png_bytes
    };

    let encoded = general_purpose::STANDARD.encode(&final_bytes);
    ToolResult::text(format!("Screenshot captured ({} bytes)", final_bytes.len()))
        .with_image(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::shared::CAPTURE_RESPONSE_TIMEOUT;
    use crate::protocol::ContentItem;
    use crate::server::McpServerConfig;
    use photonic_core::{history::CommandHistory, AuditLog, Document};
    use std::sync::{mpsc, Arc, Mutex as StdMutex};
    use tokio::sync::Mutex;

    fn state_with_capture_tx(capture_tx: mpsc::Sender<oneshot::Sender<Vec<u8>>>) -> AppState {
        AppState {
            document: Arc::new(Mutex::new(Document::new("capture test", 200.0, 100.0))),
            history: Arc::new(Mutex::new(CommandHistory::new(100))),
            document_path: Arc::new(StdMutex::new(None)),
            capture_tx: Arc::new(StdMutex::new(capture_tx)),
            config: McpServerConfig::default(),
            audit_log: Arc::new(StdMutex::new(AuditLog::new())),
            clipboard_ring: Arc::new(crate::handlers::clipboard::new_clipboard_ring()),
            ..AppState::headless_for_test()
        }
    }

    fn result_text(result: &ToolResult) -> &str {
        match result.content.first() {
            Some(ContentItem::Text { text }) => text,
            other => panic!("expected a text result, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn screenshot_times_out_when_render_loop_does_not_service_request() {
        let (capture_tx, _capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let task = tokio::spawn(async move { screenshot(&state, ScreenshotArgs::default()).await });

        tokio::task::yield_now().await;
        tokio::time::advance(CAPTURE_RESPONSE_TIMEOUT).await;
        let result = task.await.unwrap();

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("timed out"));
    }

    #[tokio::test]
    async fn screenshot_reports_request_channel_closure() {
        let (capture_tx, capture_rx) = mpsc::channel();
        drop(capture_rx);
        let state = state_with_capture_tx(capture_tx);

        let result = screenshot(&state, ScreenshotArgs::default()).await;

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("request channel is closed"));
    }

    #[tokio::test]
    async fn screenshot_reports_response_channel_closure() {
        let (capture_tx, capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let service = std::thread::spawn(move || {
            let reply_tx = capture_rx.recv().unwrap();
            drop(reply_tx);
        });

        let result = screenshot(&state, ScreenshotArgs::default()).await;
        service.join().unwrap();

        assert_eq!(result.is_error, Some(true));
        assert!(result_text(&result).contains("response channel"));
    }

    #[tokio::test]
    async fn screenshot_returns_serviced_capture() {
        let (capture_tx, capture_rx) = mpsc::channel();
        let state = state_with_capture_tx(capture_tx);
        let service = std::thread::spawn(move || {
            let reply_tx = capture_rx.recv().unwrap();
            reply_tx.send(vec![1, 2, 3]).unwrap();
        });

        let result = screenshot(&state, ScreenshotArgs::default()).await;
        service.join().unwrap();

        assert_eq!(result.is_error, None);
        assert_eq!(result_text(&result), "Screenshot captured (3 bytes)");
        assert!(matches!(
            result.content.get(1),
            Some(ContentItem::Image { .. })
        ));
    }
}

/// Decode a PNG, resize by `scale`, re-encode to PNG.
fn downscale_png(png_bytes: &[u8], scale: f32) -> Option<Vec<u8>> {
    use image::{imageops::FilterType, ImageFormat};
    let img = image::load_from_memory_with_format(png_bytes, ImageFormat::Png).ok()?;
    let new_w = ((img.width() as f32 * scale).round() as u32).max(1);
    let new_h = ((img.height() as f32 * scale).round() as u32).max(1);
    let resized = img.resize_exact(new_w, new_h, FilterType::Triangle);
    let mut out: Vec<u8> = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .ok()?;
    Some(out)
}
