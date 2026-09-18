//! Lock-free SPSC block ring: the mixer-worker → cpal-callback handoff (09 §5).
//!
//! Threading contract (02 §1, 09 §5): the mixer worker thread renders ahead
//! and pushes whole blocks; the cpal callback (real-time-safe: no locks, no
//! allocation) drains samples on demand and never blocks. Built on [`rtrb`]
//! (dual MIT/Apache-2.0) — a ring buffer purpose-built for exactly this
//! wait-free real-time-audio producer/consumer shape, chosen over hand-rolling
//! the same atomics ourselves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::RingBuffer;

use super::{BLOCK_FRAMES, CHANNELS, RING_BLOCKS};

/// Xrun telemetry shared between the [`RingProducer`] (mixer worker) and
/// [`RingConsumer`] (cpal callback) halves of one ring (09 §11's "Xrun/underrun
/// telemetry" risk row — feeds `EngineStatus.audio_xruns`, 02 §1, once the P3
/// playback controller wires it up). Counts are in **frames**, not events, for
/// finer-grained budget tracking (09 §11: "zero xruns during the SS-1
/// reference scenario" is the CI gate).
#[derive(Debug, Default)]
pub struct XrunCounters {
    /// Frames the cpal callback had to zero-fill because the ring ran dry
    /// (mixer worker fell behind).
    underrun_frames: AtomicU64,
    /// Blocks the mixer worker failed to push because the ring was full
    /// (cpal callback not draining fast enough, or not started yet).
    overrun_blocks: AtomicU64,
}

impl XrunCounters {
    pub fn underrun_frames(&self) -> u64 {
        self.underrun_frames.load(Ordering::Relaxed)
    }
    pub fn overrun_blocks(&self) -> u64 {
        self.overrun_blocks.load(Ordering::Relaxed)
    }
}

/// Producer half — owned by the mixer worker thread (09 §5). Pushes whole
/// rendered blocks (`BLOCK_FRAMES` interleaved-stereo frames); never partial.
pub struct RingProducer {
    inner: rtrb::Producer<f32>,
    xruns: Arc<XrunCounters>,
}

impl RingProducer {
    /// Push one rendered block (`block.len()` MUST be `BLOCK_FRAMES * CHANNELS`
    /// — a `debug_assert!` catches a caller mismatch in dev builds). Returns
    /// `false` (and bumps the overrun counter by one block's worth of frames)
    /// if the ring has no room; never blocks (wait-free per rtrb's guarantee),
    /// so callers own the backoff/retry policy.
    pub fn push_block(&mut self, block: &[f32]) -> bool {
        debug_assert_eq!(
            block.len(),
            BLOCK_FRAMES * CHANNELS,
            "push_block: caller must supply exactly one block"
        );
        match self.inner.push_entire_slice(block) {
            Ok(()) => true,
            Err(_) => {
                self.xruns.overrun_blocks.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Free capacity, in whole blocks — the prefill loop's "render until full"
    /// stop condition (09 §5 seek/loop: "prefill... until ring is full").
    pub fn free_blocks(&self) -> usize {
        self.inner.slots() / (BLOCK_FRAMES * CHANNELS)
    }

    pub fn is_full(&self) -> bool {
        self.inner.is_full()
    }
}

/// Consumer half — owned by the cpal callback (real-time-safe: no locks, no
/// allocation, 02 §1). Drains at whatever granularity the host's callback
/// buffer demands (not necessarily block-aligned); on underrun, the shortfall
/// is zero-filled.
pub struct RingConsumer {
    inner: rtrb::Consumer<f32>,
    xruns: Arc<XrunCounters>,
}

impl RingConsumer {
    /// Fill `out` (interleaved stereo) from the ring. If the ring runs dry
    /// mid-fill, the remainder is zero-filled and the shortfall (in frames) is
    /// added to the underrun counter.
    pub fn fill(&mut self, out: &mut [f32]) {
        let (_popped, remainder) = self.inner.pop_partial_slice(out);
        if !remainder.is_empty() {
            let missing_frames = remainder.len() / CHANNELS;
            remainder.fill(0.0);
            self.xruns
                .underrun_frames
                .fetch_add(missing_frames as u64, Ordering::Relaxed);
        }
    }
}

/// Build one [`RingProducer`]/[`RingConsumer`] pair sized to [`RING_BLOCKS`]
/// blocks, plus the [`XrunCounters`] both halves (and, typically, the P3
/// playback controller's status reporting) share.
pub fn audio_ring() -> (RingProducer, RingConsumer, Arc<XrunCounters>) {
    let capacity = BLOCK_FRAMES * CHANNELS * RING_BLOCKS;
    let (producer, consumer) = RingBuffer::<f32>::new(capacity);
    let xruns = Arc::new(XrunCounters::default());
    (
        RingProducer {
            inner: producer,
            xruns: xruns.clone(),
        },
        RingConsumer {
            inner: consumer,
            xruns: xruns.clone(),
        },
        xruns,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_block(fill: f32) -> Vec<f32> {
        vec![fill; BLOCK_FRAMES * CHANNELS]
    }

    #[test]
    fn push_pop_roundtrips_exact_block() {
        let (mut prod, mut cons, _xruns) = audio_ring();
        assert!(prod.push_block(&one_block(0.5)));
        let mut out = vec![-1.0; BLOCK_FRAMES * CHANNELS];
        cons.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn overrun_counter_increments_when_ring_full() {
        let (mut prod, _cons, xruns) = audio_ring();
        // Capacity is exactly RING_BLOCKS blocks.
        for _ in 0..RING_BLOCKS {
            assert!(prod.push_block(&one_block(0.0)));
        }
        assert!(prod.is_full());
        assert!(
            !prod.push_block(&one_block(0.0)),
            "ring is full, push must fail"
        );
        assert_eq!(xruns.overrun_blocks(), 1);
    }

    #[test]
    fn underrun_counter_increments_on_empty_ring() {
        let (_prod, mut cons, xruns) = audio_ring();
        let mut out = vec![-1.0; BLOCK_FRAMES * CHANNELS];
        cons.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.0), "empty ring must zero-fill");
        assert_eq!(xruns.underrun_frames(), BLOCK_FRAMES as u64);
    }

    #[test]
    fn partial_fill_reports_exact_shortfall() {
        let (mut prod, mut cons, xruns) = audio_ring();
        // Push half a block's worth of frames via a smaller producer push is not
        // possible with push_block's whole-block contract, so push one full
        // block and drain more than that in one `fill` call to force a partial.
        assert!(prod.push_block(&one_block(1.0)));
        let mut out = vec![-1.0; BLOCK_FRAMES * CHANNELS * 2];
        cons.fill(&mut out);
        assert!(out[..BLOCK_FRAMES * CHANNELS].iter().all(|&s| s == 1.0));
        assert!(out[BLOCK_FRAMES * CHANNELS..].iter().all(|&s| s == 0.0));
        assert_eq!(xruns.underrun_frames(), BLOCK_FRAMES as u64);
    }

    #[test]
    fn free_blocks_tracks_capacity() {
        let (mut prod, _cons, _xruns) = audio_ring();
        assert_eq!(prod.free_blocks(), RING_BLOCKS);
        prod.push_block(&one_block(0.0));
        assert_eq!(prod.free_blocks(), RING_BLOCKS - 1);
    }
}
