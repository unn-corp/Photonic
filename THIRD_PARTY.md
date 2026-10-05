# Third-party implementation intake

Photonic may adopt open-source components that permit commercial sale and paid
services when the exact distribution complies with their terms. MIT is not a
requirement. The current policy is in
`docs/specs/video-editor/23-legal-open-source-implementation-routes.md` §3.2;
the preliminary dependency and FFmpeg findings are in
`docs/implementation/commercial-license-review.md`.

OpenColorIO 2.5.2 was evaluated in an isolated experiment. Its main BSD-style
license and included SampleICC notice appear to permit commercial use, subject
to attribution and non-endorsement conditions. The experimental probe was
removed during the brief MIT-only interpretation. No OCIO runtime, config,
shader, or generated artifact is bundled. The owner selected Photonic-owned
Rust/WGSL transforms instead of OCIO runtime adoption. The deferred proposal
and its would-be approval requirements are in
`docs/implementation/ocio-runtime-decision.md`.

## ACES 2 output-math reference

- Component: Academy Software Foundation `aces-core`,
  `lib/Lib.Academy.Tonescale.ctl` and the JMh model equations in
  `lib/Lib.Academy.OutputTransform.ctl`.
- Upstream: https://github.com/aces-aswf/aces-core
- Revision: `069b0bc3e1f6c62820f19fdae2fecec3f4fc0f80`.
- Referenced source SHA-256:
  tonescale `fcb99aad7fdf52ae048886171c18af9107ceeee2709aa33233ef09636fd1c6c4`;
  output model `34a23e14e29dff47f5575514b7c98f502e2be7784a81709a9ca9b53777b2fdfe`.
- SDR target preset: `aces-output` revision
  `6d8f9071a67b044bac0fbcb3d51ad0543f065e66`, file
  `d65/srgb/Output.Academy.Rec709-D65_100nit_in_Rec709-D65_sRGB-Piecewise.ctl`,
  SHA-256 `3b750b94a46c936f3ff83e29acc1254700d96b0ac8dca9095060a3c5c9db6a65`;
  Apache-2.0. Its documented Rec.709/D65, 100-nit, sRGB-piecewise parameters
  identify the target only; the CTL preset is not bundled.
- License: Apache-2.0, copyright Contributors to the ACES Project. Preserve
  the file-level license and copyright notice for the adapted Rust component;
  the full text is in `licenses/Apache-2.0.txt`. The `photonic-video` package
  declares `MIT AND Apache-2.0` for its combined distribution obligations.
- Scope: mathematical tone-scale and JMh appearance-coordinate components,
  in-gamut chroma shaping, a display-cube cusp search, a corner-aware scalar
  gamut compressor with fitted hull/focus-line math, and isolated 100-nit
  SDR scalar and WGSL output APIs, plus independent numerical checks. No upstream CTL
  file, interpreter, executable, LUT, image, or other
  asset is bundled; no new Cargo dependency or build script is introduced.
- Validation table: [Academy tone-mapping documentation](https://docs.acescentral.com/system-components/output-transforms/technical-details/tone-mapping/),
  distributed under CC BY 4.0. The source is linked alongside the numerical
  test vectors; no documentation prose or image is copied.
- Enabled features/transitives: none. Maintenance owner: Photonic color
  pipeline maintainers. Security review: pure bounded scalar math; no I/O or
  unsafe code. Patent/trademark review remains a release gate; ACES branding
  is used only to identify the published transform, with no endorsement claim.
- Intended use: isolated development implementation while the complete
  managed output path remains disabled. Commercial distribution of the final
  color pipeline requires the exact shipped-code and notice audit described
  in the implementation policy.
- Photonic SDR v1's fixed-lightness/hue Rec.709 boundary search and smooth
  shoulder are original Photonic policy layered after the attributed appearance
  and tone-scale components. It is not represented as the Academy ACES 2
  Output Transform.

## ACES 2 output qualification fixture

- Test-only source: [OpenColorIO-Config-ACES v4.0.0](https://github.com/AcademySoftwareFoundation/OpenColorIO-Config-ACES/releases/tag/v4.0.0),
  `cg-config-v4.0.0_aces-v2.0_ocio-v2.5.ocio`, SHA-256
  `9e3ec773a7fbc0bb6666e428dedd8ccec4f59b8b6d40506c825cc7a8c02ff2ff`.
- License: BSD-3-Clause, as stated in the [upstream README](https://github.com/AcademySoftwareFoundation/OpenColorIO-Config-ACES/blob/v4.0.0/README.rst).
  The exact upstream notice is preserved in
  `licenses/BSD-3-Clause-ACES-Config.txt` (SHA-256
  `82a40c52065ee968aa62015735e84378ac11425db7a233e6a2dcd2c83fc24276`).
  The config was run with the local OCIO 2.5.1 executable against Photonic-owned
  synthetic float EXR pixels to produce twelve hand-selected vectors, a
  64-sample RGB grid, and 32 additional corner/highlight/negative samples.
  The numerical grids are included as test fixtures at
  `tests/fixtures/aces2_sdr_100nit_4cube.tsv` (SHA-256
  `53e91d4968387b100fcef28991653d8ccfc1c82247c73acaca4b10722ce5ae20`)
  and `tests/fixtures/aces2_sdr_100nit_edges.tsv` (SHA-256
  `afd6d4c58ff1a5fe78e18ebcbe325e4700eb12506c7518346358cd3a17fd7ba8`).
  This is `VALIDATE` use under the repository policy; neither executable,
  config, nor EXR is shipped or used by Photonic at runtime. Source packages
  containing the numerical fixture must retain this provenance and notice.
- The older Photonic SDR v1 candidate fails its chromatic qualification tests;
  the separate ACES 2-style scalar and isolated GPU paths pass the sampled
  references. Managed preview/export remain gated pending image-level,
  workflow and performance qualification.
  The vectors are a benchmark, not a claim of complete Academy output-transform
  conformance. Security surface is numeric fixture parsing in tests only;
  maintenance owner is Photonic color pipeline maintainers. No new runtime
  dependency or asset is introduced.
