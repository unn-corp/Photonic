//! Owner-aware native analysis scheduler; provisional graphs never reach delivery.
use super::*;
use crate::graph::deflicker::{ClipGains, NativeDeflickerKey};
use crate::graph::native_deflicker_plan::NativeDeflickerPlan;
use std::hash::{Hash, Hasher};

#[derive(Default)]
pub(super) struct NativeAnalysisRuntime {
    identity: Option<u64>,
    plans: Vec<NativeDeflickerPlan>,
    done: HashSet<NativeDeflickerKey>,
    current: Option<NativeJob>,
    next: Option<usize>,
    dependency_stack: Vec<NativeDeflickerKey>,
    started: Option<Instant>,
    error: Option<String>,
}
struct NativeJob {
    plan: NativeDeflickerPlan,
    samples: Vec<Option<[f32; 3]>>,
    covered: Vec<bool>,
    burst_ticks: Vec<Tick>,
    burst: Vec<Vec<f32>>,
    prepared: bool,
    solved: Vec<[f32; 3]>,
    cache: Option<((usize, usize, u32), Vec<[f32; 3]>)>,
}
impl NativeJob {
    fn new(plan: NativeDeflickerPlan) -> Self {
        let burst_ticks = plan.burst_ticks();
        Self {
            plan,
            samples: Vec::new(),
            covered: Vec::new(),
            burst_ticks,
            burst: Vec::new(),
            prepared: false,
            solved: Vec::new(),
            cache: None,
        }
    }
}

/// Empty transparent intervals are not missing decodes. Extend the nearest
/// observed exposure through them so a gap cannot become synthetic mid-grey.
fn fill_empty_exposure(samples: &mut [Option<[f32; 3]>], covered: &[bool]) -> bool {
    if samples.len() != covered.len() || samples.iter().any(Option::is_none) {
        return false;
    }
    let observed: Vec<_> = covered
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.then_some(i))
        .collect();
    if observed.is_empty() {
        samples.fill(Some([0.5; 3]));
        return true;
    }
    for i in 0..samples.len() {
        if !covered[i] {
            let nearest = *observed.iter().min_by_key(|j| i.abs_diff(**j)).unwrap();
            samples[i] = samples[nearest];
        }
    }
    true
}

impl EngineThread {
    fn native_analysis_identity(&self, project: &TimelineProject, sequence: SequenceId) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sequence.hash(&mut h);
        project
            .sequences
            .get(&sequence)
            .map(|s| s.active_format)
            .hash(&mut h);
        self.media.invalidation_epoch().hash(&mut h);
        if self
            .snapshot
            .as_ref()
            .is_some_and(|p| std::ptr::eq(p.as_ref(), project))
        {
            self.snapshot_generation.hash(&mut h);
            (project as *const TimelineProject as usize).hash(&mut h);
        } else {
            // Direct internal test callers may mutate a stack-owned project;
            // production snapshots use the cheap generation/Arc identity above.
            format!("{project:?}").hash(&mut h);
        }
        h.finish()
    }
    pub(super) fn native_analysis_pending(
        &self,
        project: &TimelineProject,
        sequence: SequenceId,
    ) -> bool {
        let state = &self.native_deflicker_runtime;
        state.identity != Some(self.native_analysis_identity(project, sequence))
            || (state.error.is_none()
                && (state.current.is_some() || state.done.len() < state.plans.len()))
    }
    pub(super) fn native_analysis_error(&self) -> Option<&str> {
        self.native_deflicker_runtime.error.as_deref()
    }
    fn fail_native_analysis(&mut self, message: impl Into<String>) {
        self.native_deflicker_runtime.error = Some(message.into());
        self.native_deflicker_runtime.current = None;
        self.native_deflicker_runtime.next = None;
        self.controller.request_present();
    }
    pub(super) fn advance_native_analysis(
        &mut self,
        project: &TimelineProject,
        sequence: SequenceId,
        format_index: usize,
    ) {
        let identity = self.native_analysis_identity(project, sequence);
        if self.native_deflicker_runtime.identity != Some(identity) {
            self.deflicker_gains.clear_native();
            self.native_deflicker_runtime = NativeAnalysisRuntime {
                identity: Some(identity),
                started: Some(Instant::now()),
                ..Default::default()
            };
            match crate::graph::native_deflicker_plan::collect(project, sequence) {
                Ok(plans) => self.native_deflicker_runtime.plans = plans,
                Err(error) => {
                    self.fail_native_analysis(error);
                    return;
                }
            }
        }
        if self.native_deflicker_runtime.error.is_some() {
            return;
        }
        if self.native_deflicker_runtime.done.len() == self.native_deflicker_runtime.plans.len() {
            return;
        }
        if self
            .native_deflicker_runtime
            .started
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(20))
        {
            self.fail_native_analysis("native Deflicker analysis exceeded its20second preparation deadline; reduce enabled analysis scopes or retry after source recovery");
            return;
        }
        if self.native_deflicker_runtime.current.is_none() {
            let state = &mut self.native_deflicker_runtime;
            let index = state.next.take().or_else(|| {
                state
                    .plans
                    .iter()
                    .position(|p| !state.done.contains(&p.key))
            });
            let Some(index) = index else {
                return;
            };
            state.current = Some(NativeJob::new(state.plans[index].clone()));
        }
        let mut job = self.native_deflicker_runtime.current.take().unwrap();
        if job.samples.len() == job.plan.grid.ticks.len()
            && job.burst.len() == job.burst_ticks.len()
        {
            self.solve_native_owner_job(job);
            return;
        }
        let exposure = job.samples.len() < job.plan.grid.ticks.len();
        let local = if exposure {
            job.plan.grid.ticks[job.samples.len()]
        } else {
            job.burst_ticks[job.burst.len()]
        };
        let Some(at) = job.plan.sequence_start.0.checked_add(local.0) else {
            self.fail_native_analysis("native analysis clock overflow");
            return;
        };
        let compiled = match crate::graph::compile::compile_native_deflicker_input(
            project,
            job.plan.key.sequence,
            format_index,
            Tick(at),
            job.plan.key,
            &self.lut_cache,
            &self.deflicker_gains,
            Some(&self.media.native_source_errors),
            true,
        ) {
            Ok(frame) => frame,
            Err(crate::graph::compile::NativeDeflickerInputError::Invalid(error)) => {
                self.fail_native_analysis(error);
                return;
            }
            Err(crate::graph::compile::NativeDeflickerInputError::Pending(deps)) => {
                let dependency = deps
                    .into_iter()
                    .find(|key| !self.native_deflicker_runtime.done.contains(key));
                let Some(dependency) = dependency else {
                    self.fail_native_analysis(
                        "native analysis dependency has no valid measurement",
                    );
                    return;
                };
                if dependency == job.plan.key
                    || self
                        .native_deflicker_runtime
                        .dependency_stack
                        .contains(&dependency)
                {
                    self.fail_native_analysis("native Deflicker analysis dependency cycle");
                    return;
                }
                let Some(index) = self
                    .native_deflicker_runtime
                    .plans
                    .iter()
                    .position(|p| p.key == dependency)
                else {
                    self.fail_native_analysis(
                        "native Deflicker input depends on an unavailable owner/context plan",
                    );
                    return;
                };
                self.native_deflicker_runtime
                    .dependency_stack
                    .push(job.plan.key);
                self.native_deflicker_runtime.next = Some(index);
                return;
            }
        };
        let rendered = self
            .evaluator
            .evaluate_with_tap_frame(&compiled.graph, (320, 180), &mut self.media, None)
            .0;
        if !self.evaluator.computation_errors().is_empty() {
            self.fail_native_analysis(format!(
                "native Deflicker input failed: {}",
                self.evaluator.computation_errors().join("; ")
            ));
            return;
        }
        if self.evaluator.pending_computations() > 0 {
            self.native_deflicker_runtime.current = Some(job);
            return;
        }
        if let Some(frame) = rendered {
            if frame.width == 0 || frame.height == 0 || frame.width > 640 || frame.height > 360 {
                self.fail_native_analysis(
                    "native analysis image exceeds bounded sample dimensions",
                );
                return;
            }
            let image = crate::graph::ops::Image {
                width: frame.width,
                height: frame.height,
                pixels: crate::graph::eval::read_texture_rgba16f(
                    self.evaluator.gpu(),
                    &frame.texture,
                    frame.width,
                    frame.height,
                ),
            };
            if image.pixels.len() != (frame.width * frame.height) as usize
                || !image.pixels.iter().flatten().all(|v| v.is_finite())
            {
                self.fail_native_analysis("native analysis produced invalid pixels");
                return;
            }
            let covered = image.pixels.iter().any(|p| p[3] > 1e-6);
            let meter = crate::graph::deflicker::native_meter_image(&image);
            if exposure {
                job.samples
                    .push(Some(crate::graph::deflicker::measure_frame(&meter)));
                job.covered.push(covered);
            } else {
                job.burst
                    .push(crate::graph::rolling_bands::row_profile(&meter));
            }
        }
        // Cold sources keep the exact slot pending; never advance past a miss.
        self.native_deflicker_runtime.current = Some(job);
    }
    fn solve_native_owner_job(&mut self, mut job: NativeJob) {
        if !job.prepared {
            if !fill_empty_exposure(&mut job.samples, &job.covered) {
                self.fail_native_analysis("native analysis curve is incomplete");
                return;
            }
            job.prepared = true;
        }
        let index = if job.plan.animated {
            job.solved.len()
        } else {
            0
        };
        let Some(at) = job.plan.first.0.checked_add(job.plan.grid.ticks[index].0) else {
            self.fail_native_analysis("native analysis parameter clock overflow");
            return;
        };
        let Some(params) = crate::graph::deflicker::native_analysis_params_at(
            &job.plan.authored,
            Tick(at),
            job.plan.fps,
            job.plan.grid.stride,
        ) else {
            self.fail_native_analysis(
                "native analysis controls are outside their supported ranges",
            );
            return;
        };
        let key = (
            params.window_frames,
            params.smooth_frames,
            params.max_change.to_bits(),
        );
        if job.cache.as_ref().is_none_or(|(cached, _)| *cached != key) {
            let Some(curve) = job.plan.grid.solve_complete_native(&job.samples, params) else {
                self.fail_native_analysis("native analysis curve is incomplete or invalid");
                return;
            };
            job.cache = Some((key, curve.gains));
        }
        let gains = if job.plan.animated {
            job.solved.push(job.cache.as_ref().unwrap().1[index]);
            if job.solved.len() < job.plan.grid.ticks.len() {
                self.native_deflicker_runtime.current = Some(job);
                return;
            }
            std::mem::take(&mut job.solved)
        } else {
            std::mem::take(&mut job.cache.as_mut().unwrap().1)
        };
        let anchor = job
            .burst_ticks
            .first()
            .map_or(0, |t| t.0 / job.plan.frame_ticks);
        let band = crate::graph::rolling_bands::track_from_burst(&job.burst, job.plan.fps, anchor);
        let curve = ClipGains {
            first: job.plan.first,
            step: job.plan.grid.step,
            gains,
            band: None,
            frame_ticks: job.plan.frame_ticks,
        }
        .with_band(band, job.plan.frame_ticks);
        self.deflicker_gains.insert_native(job.plan.key, curve);
        self.native_deflicker_runtime.done.insert(job.plan.key);
        self.native_deflicker_runtime.dependency_stack.clear();
        self.controller.request_present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_deflicker_transparent_gaps_do_not_invent_midgrey_exposure() {
        let mut values = vec![
            Some([0.2; 3]),
            Some([0.5; 3]),
            Some([0.5; 3]),
            Some([0.2; 3]),
        ];
        assert!(fill_empty_exposure(
            &mut values,
            &[true, false, false, true]
        ));
        assert_eq!(values, vec![Some([0.2; 3]); 4]);
        let mut missing = vec![Some([0.2; 3]), None];
        assert!(!fill_empty_exposure(&mut missing, &[true, false]));
        let mut empty = vec![Some([0.0; 3]); 4];
        assert!(fill_empty_exposure(&mut empty, &[false; 4]));
        assert_eq!(empty, vec![Some([0.5; 3]); 4]);
        let mut edge = vec![
            Some([0.5; 3]),
            Some([0.3; 3]),
            Some([0.7; 3]),
            Some([0.5; 3]),
        ];
        assert!(fill_empty_exposure(&mut edge, &[false, true, true, false]));
        assert_eq!(
            edge,
            vec![
                Some([0.3; 3]),
                Some([0.3; 3]),
                Some([0.7; 3]),
                Some([0.7; 3])
            ]
        );
    }
}
