# OCIO runtime decision record

Status: **Photonic-owned runtime selected; OCIO proposal deferred**. On
2026-09-23 the project owner clarified that MIT is not required if the adopted
license permits commercial sale and paid services and Photonic can meet its
conditions. The published OpenColorIO BSD-style license and the included
SampleICC ICC Software License v0.2 permit redistribution and use in source
and binary form; neither states a commercial-use ban. Both require notices,
and the ICC terms prohibit implied endorsement. The ICC membership sentence
encourages membership for commercial use; it does not state a requirement.
This is a reading of the published terms, not approval of the complete build.
No legal or architecture decision for the custom license is recorded. The
project owner chose the Photonic-owned color route on 2026-09-23, so no OCIO
runtime integration is planned under the current roadmap.

Legacy SDR remains the default. Existing version-1 OCIO-named project metadata
must be preserved on load and must fail closed in this build. The selected
implementation is Photonic-owned Rust/WGSL transforms, with the existing
`moxcms` ICC path retained. The unused OCIO proposal below is preserved as a
decision record; reviving it would require a new decision and the gates below.

## Deferred proposal

Use OpenColorIO 2.5.2 as an isolated native color-transform service for managed
sequences. Pin the upstream source archive to SHA-256
`722601e01b78b7a12da4829cb450674935f404b0e508f3f20046fa77570e3272`.
Use the named builtin `cg-config-v2.2.0_aces-v1.3_ocio-v2.4` initially. Preserve
Legacy SDR as the default. Reject unsupported GPU processors rather than changing
the image silently. The proposed service has no API that accepts arbitrary
SPIR-V or modifies the Photonic document directly.

## Evidence completed

- The main OCIO license is BSD-3-Clause. Its upstream `THIRD-PARTY.md` includes
  the **ICC Software License v0.2** for SampleICC and other notices. The selected
  core CMake target includes `FileFormatICC.cpp` and links `sampleicc`; the custom
  notice is applicable even though the development probe uses no ICC input.
- An isolated 2.5.2 build with external packages disabled passed a CPU/shader
  probe. Eleven ACEScct encodings matched the published equation. Three selected
  processors passed OCIO CPU versus Vulkan/wgpu comparisons on the recorded
  local adapter. The experimental probe sources and qualification artifacts
  were removed after the MIT-only decision; these are historical measurements.
- 2.5.2 fixes the upstream LUT parser vulnerability CVE-2026-42450. The probe
  reads only a builtin configuration and owned synthetic samples.
- The initial native build uses local system expat, yaml-cpp, pystring, Imath,
  zlib-compatible and minizip-ng. This is development evidence, not a portable
  release SBOM. No OCIO binary, config, generated shader, or native code is
  bundled in Photonic yet.

## Decision that would be required if OCIO were reconsidered

Approve or reject the **runtime use and redistribution** of OpenColorIO 2.5.2
including the applicable SampleICC ICC Software License v0.2 terms and notices.
The review must address attribution, non-endorsement wording, native ownership,
security maintenance and the optional native adapter boundary. The repository
policy in `docs/specs/video-editor/23-legal-open-source-implementation-routes.md`
§3.2 requires written legal and architecture approval for custom licenses.

Approval of this use would permit runtime implementation work; release remains
gated separately on a complete transitive SBOM and notices, reproducible Linux/
Windows/macOS builds, package and signature checks, texture/uniform shader
qualification, high-precision decode, end-to-end color tests and maintenance
ownership. Record reviewers, date, exact approved scope and conditions here.

Legal reviewer: **none designated**  
Architecture reviewer: **none designated**  
Decision: **not requested for the selected native route; no OCIO runtime or release artifact approved**  
Scope under review: **Photonic runtime and release packages**  
Required record: **named reviewers, date, exact scope, conditions, and transitive build evidence**

## Selected route avoiding this custom-license decision

- Implement the managed ACEScg/ACEScct and SDR display/export transforms in
  Photonic-owned Rust/WGSL, using published specifications and owned reference
  vectors. This avoids adopting OCIO and SampleICC, but still requires color
  correctness and release qualification before managed rendering is enabled.
- Photonic already uses commercially usable `moxcms` for ICC profile
  transforms. Retain and audit that existing dependency rather than add a
  second ICC engine. ICC conversion alone does not provide OCIO's ACES
  configuration, grading graph, display/view, or GPU processor workflow.

Neither route removes the ordinary dependency, asset, patent, security, and
packaging checks. The alternative is to complete the written custom-license
and architecture decision above before integrating OCIO.
