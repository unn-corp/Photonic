//! GPU capability floor for video mode (37 §1.2).
//!
//! Video editing leans on a handful of features that vector editing does not:
//! a filterable `Rgba16Float` render target (the linear-light working format
//! every grade/composite pass writes into), textures large enough for a 4K+
//! frame, compute + storage textures (scopes, histogram, waveform reductions),
//! and non-tiny storage-buffer bindings. Below that floor the video path cannot
//! run correctly, so we *refuse video mode* and keep vector mode alive rather
//! than limp along and produce wrong pixels.
//!
//! The check is deliberately **adapter-only and pure**: it needs no device, so
//! it can gate *before* `request_device`, and it never panics on any adapter —
//! a missing capability is reported, not asserted. The actual policy split
//! (refuse video, keep vector) lives in `photonic-gui`; this module only
//! answers "does this adapter clear the floor, and if not, why".
//!
//! **Non-uniform buffer sizes choice:** wgpu 22 exposes no direct
//! "non-uniform buffer size" query, so we use
//! [`wgpu::DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED`] — the closest
//! true proxy for a backend that does not force 16-byte-aligned binding sizes.

/// A single capability the video path requires of the GPU adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityRequirement {
    /// `Rgba16Float` usable as a filterable render target (the linear working
    /// format for every grade/composite pass).
    Rgba16FloatFilterableRenderTarget,
    /// `max_texture_dimension_2d` at least `required` (a 4K+ frame plus the
    /// padded intermediate targets derived from it).
    MaxTextureDimension2d { required: u32 },
    /// Compute shaders plus at least one storage texture per stage (scopes,
    /// histogram and waveform reductions run as compute passes).
    ComputeAndStorageTextures,
    /// Storage-buffer bindings that are not forced to 16-byte alignment.
    NonUniformBufferSizes,
}

impl CapabilityRequirement {
    /// One human sentence naming the missing capability.
    fn describe(&self) -> String {
        match self {
            Self::Rgba16FloatFilterableRenderTarget => {
                "GPU cannot use Rgba16Float as a filterable render target".to_string()
            }
            Self::MaxTextureDimension2d { required } => {
                format!("GPU max 2D texture dimension is below the required {required}")
            }
            Self::ComputeAndStorageTextures => {
                "GPU lacks compute shaders with storage textures".to_string()
            }
            Self::NonUniformBufferSizes => {
                "GPU forces 16-byte-aligned storage buffer binding sizes".to_string()
            }
        }
    }
}

/// The outcome of a capability-floor check: the list of requirements the
/// adapter failed to meet. An empty list means the adapter clears the floor.
#[derive(Clone, Debug, Default)]
pub struct CapabilityReport {
    /// Requirements the adapter did NOT satisfy. Empty => floor met.
    pub missing: Vec<CapabilityRequirement>,
}

impl CapabilityReport {
    /// True when the adapter satisfies every requirement.
    pub fn meets_floor(&self) -> bool {
        self.missing.is_empty()
    }

    /// A human-readable reason string — one sentence per missing capability,
    /// suitable to surface verbatim in a "video mode unavailable" notice.
    /// Empty string when the floor is met.
    pub fn reason(&self) -> String {
        self.missing
            .iter()
            .map(CapabilityRequirement::describe)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Minimum `max_texture_dimension_2d` the video path requires (a 4K frame plus
/// the padded intermediate targets derived from it).
pub const MIN_TEXTURE_DIMENSION_2D: u32 = 8192;

/// Check the GPU capability floor against an adapter.
///
/// Pure and adapter-only — creates no device and never panics — so it can gate
/// video mode *before* `request_device`.
pub fn check_capability_floor(adapter: &wgpu::Adapter) -> CapabilityReport {
    let limits = adapter.limits();
    let downlevel = adapter.get_downlevel_capabilities().flags;
    let rgba16 = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba16Float);
    floor_from_parts(&limits, downlevel, rgba16)
}

/// The pure decision function, factored out of [`check_capability_floor`] so it
/// is unit-testable with hand-built inputs and no adapter/device.
pub fn floor_from_parts(
    limits: &wgpu::Limits,
    downlevel: wgpu::DownlevelFlags,
    rgba16: wgpu::TextureFormatFeatures,
) -> CapabilityReport {
    let mut missing = Vec::new();

    let rgba16_ok = rgba16
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
        && rgba16
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE);
    if !rgba16_ok {
        missing.push(CapabilityRequirement::Rgba16FloatFilterableRenderTarget);
    }

    if limits.max_texture_dimension_2d < MIN_TEXTURE_DIMENSION_2D {
        missing.push(CapabilityRequirement::MaxTextureDimension2d {
            required: MIN_TEXTURE_DIMENSION_2D,
        });
    }

    let compute_ok = downlevel.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && limits.max_storage_textures_per_shader_stage >= 1;
    if !compute_ok {
        missing.push(CapabilityRequirement::ComputeAndStorageTextures);
    }

    if !downlevel.contains(wgpu::DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED) {
        missing.push(CapabilityRequirement::NonUniformBufferSizes);
    }

    CapabilityReport { missing }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `TextureFormatFeatures` that clears the Rgba16Float requirement.
    fn good_rgba16() -> wgpu::TextureFormatFeatures {
        wgpu::TextureFormatFeatures {
            allowed_usages: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            flags: wgpu::TextureFormatFeatureFlags::FILTERABLE,
        }
    }

    /// `DownlevelFlags` that clear every downlevel-gated requirement.
    fn good_downlevel() -> wgpu::DownlevelFlags {
        wgpu::DownlevelFlags::COMPUTE_SHADERS
            | wgpu::DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED
    }

    /// `Limits` that clear every limit-gated requirement.
    fn good_limits() -> wgpu::Limits {
        wgpu::Limits {
            max_texture_dimension_2d: MIN_TEXTURE_DIMENSION_2D,
            max_storage_textures_per_shader_stage: 1,
            ..Default::default()
        }
    }

    #[test]
    fn all_satisfied_meets_floor() {
        let report = floor_from_parts(&good_limits(), good_downlevel(), good_rgba16());
        assert!(report.meets_floor(), "reason: {}", report.reason());
        assert!(report.missing.is_empty());
    }

    #[test]
    fn small_texture_dimension_is_the_only_miss() {
        let mut limits = good_limits();
        limits.max_texture_dimension_2d = 4096;
        let report = floor_from_parts(&limits, good_downlevel(), good_rgba16());
        assert!(!report.meets_floor());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(
            report.missing[0],
            CapabilityRequirement::MaxTextureDimension2d {
                required: MIN_TEXTURE_DIMENSION_2D
            }
        );
        assert!(
            report.reason().contains("8192"),
            "reason should mention the required dimension: {}",
            report.reason()
        );
    }

    #[test]
    fn cleared_compute_flag_misses_compute_and_storage() {
        let downlevel = wgpu::DownlevelFlags::BUFFER_BINDINGS_NOT_16_BYTE_ALIGNED;
        let report = floor_from_parts(&good_limits(), downlevel, good_rgba16());
        assert!(!report.meets_floor());
        assert!(report
            .missing
            .contains(&CapabilityRequirement::ComputeAndStorageTextures));
    }

    #[test]
    fn non_filterable_rgba16_misses_render_target() {
        let rgba16 = wgpu::TextureFormatFeatures {
            allowed_usages: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            flags: wgpu::TextureFormatFeatureFlags::empty(), // not FILTERABLE
        };
        let report = floor_from_parts(&good_limits(), good_downlevel(), rgba16);
        assert!(!report.meets_floor());
        assert!(report
            .missing
            .contains(&CapabilityRequirement::Rgba16FloatFilterableRenderTarget));
    }
}
