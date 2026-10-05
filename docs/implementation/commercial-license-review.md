# Commercial-use and distribution license review

Status: **preliminary inventory, not release approval**. On 2026-09-23 the
project owner clarified that Photonic may use non-MIT open-source components
when their terms permit selling the product and offering paid services and the
release meets those terms. Commercial permission is distinct from the right to
ship a particular binary, configuration, asset, or model without conditions.

The current `Cargo.lock` resolves 751 third-party packages across all workspace
targets/features. A manifest-level scan finds 76 without an MIT option. **That
is not a list of commercially prohibited packages.** Direct examples include
Apache-2.0 `cpal`, `fastembed`, and `winit`; BSD-3-Clause `subtle`; BSD-3-Clause
OR Apache-2.0 `moxcms`; and BSL-1.0 `xxhash-rust`. The existing
`cargo deny check licenses` passes. Actual license files, selected features,
notices, bundled data, and target-specific graphs still need release review.

| License/component | Commercial path | Release obligation or open gate |
| --- | --- | --- |
| MIT, BSD-2/3, ISC, Zlib, Apache-2.0 | Generally permit commercial use and sale. | Keep copyright/license notices; Apache-2.0 may require NOTICE and change notices. BSD-3 prohibits implied endorsement. Verify each adopted file and dependency. |
| MPL-2.0 (`option-ext`) | Commercial use and inclusion in a larger work are permitted. | File-level source and modification obligations apply to distributed MPL-covered files. Keep its current exception only with those obligations recorded in release materials. |
| Font, certificate, model, LUT and other data licenses | Must be assessed separately from the containing crate's code license. | Verify redistribution, modification, attribution, and commercial rights for the exact bundled bytes. |
| OpenColorIO 2.5.2 | Its BSD-style main license and SampleICC's ICC Software License v0.2 permit source/binary redistribution and use; no commercial-use ban was found in their published terms. | Reproduce notices and avoid implied ICC endorsement. Custom-license, transitive-native, architecture, and reproducible packaging review remain open. No runtime integration is approved yet. |
| FFmpeg sidecar | FFmpeg's LGPL/GPL licenses allow commercial activity. | The exact distributed build controls obligations. The local `ffmpeg n9.0.1` has `--enable-gpl`, `--enable-libx264`, and `--enable-libx265`; it is **not** a qualified redistributable Photonic sidecar. Pin and audit a release build/configuration and satisfy the applicable source/notice terms before bundling. |

Paths that avoid **bundling this workstation's GPL-enabled FFmpeg** are (a)
require a separately installed/user-supplied executable and make the feature
and its limits explicit, or (b) qualify and distribute a pinned FFmpeg build
with GPL and nonfree features disabled under its applicable LGPL terms. The
first gives up a self-contained install and still needs subprocess/security
review; the second has LGPL distribution duties and the repository's written
review gate. A Photonic-owned or permissively licensed codec/container stack
could eventually replace the sidecar, but no full-format, drop-in replacement
has been qualified. A single-purpose AV1 encoder such as BSD-2-Clause `rav1e`
does not cover probing, decoding, containers, audio and broad export.
Photonic's locator now treats `PHOTONIC_FFMPEG_DIR` as an authoritative
user-supplied directory and requires `ffmpeg` and `ffprobe` to coexist there.
Without an override, it chooses the first directory on `PATH` containing both.
It cannot silently mix binaries from two installs or substitute a system build
when the explicit selection is incomplete. This changes discovery only; it
does not certify any chosen FFmpeg build for redistribution.
Multi-job export now reports a missing or invalid external FFmpeg installation
in the Render Queue instead of silently discarding the submitted jobs. The
`get_media_toolchain_status` MCP tool reports the selected executable pair or
the same setup error without requiring a running video engine;
`get_engine_status.media_tools` includes it during an editing session. Neither
implies that the workstation binary is part of the Photonic release.

The repository's `deny.toml` is a useful Cargo license gate, but it does not
inspect an external FFmpeg executable, system libraries, fonts/images/models,
every optional feature, trademark claims, or patent rights. CI's system FFmpeg
is a test dependency, not evidence of an approved shipping binary.

Before a commercial release, inventory the exact Linux/Windows/macOS artifacts
and every bundled asset, select each dual-license option, generate a transitive
SBOM and notices, document source-availability duties for copyleft components,
pin the sidecar build flags and codecs, and run packaging/reimport tests. Hold
or replace only components whose exact terms or feasible compliance path fail
that review.

Primary references: [OSI Open Source Definition](https://opensource.org/osd),
[Apache-2.0](https://opensource.org/license/apache-2.0),
[BSD-3-Clause](https://opensource.org/license/BSD-3-clause),
[Mozilla MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/),
[OpenColorIO 2.5.2 notices](https://github.com/AcademySoftwareFoundation/OpenColorIO/blob/v2.5.2/THIRD-PARTY.md),
and [FFmpeg license guidance](https://ffmpeg.org/legal.html).
