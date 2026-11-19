//! Frame-time gate on the adapter the app picks (`docs/design/PERF_3D.md`, «Measured»).
//! Limits are about twice the numbers measured on the reference laptop GPU (GeForce MX250,
//! DX12), so a regression of the kind fixed here (shader compiles on the first frame, a stalled
//! refinement) fails, while run-to-run noise does not.

use ddc_render::synth::{self, SynthSpec};
use ddc_render::Quality;

const SIZE: (u32, u32) = (1280, 720);
const FRAMES: u32 = 60;

/// Renderer and first-frame limits, ms: pipelines are compiled side by side at creation.
/// Until the first measured run, the limits are the PERF_3D targets for a far larger model.
const SETUP_MS: f64 = 3000.0;
const FIRST_FRAME_MS: f64 = 1000.0;
/// 10 000 elements, ~1.1 M triangles.
const MOVING_P95_MS: f64 = 50.0;
const STILL_P95_MS: f64 = 100.0;
/// Refinement of a still camera, 10 000 elements.
const SETTLE_MS: f64 = 10_000.0;
