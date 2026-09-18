//! Deterministic CPU reference for still-panorama projection.

use std::f32::consts::{PI, TAU};

use glam::{Mat3, Vec3};
use thiserror::Error;

use super::ops::Image;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanoramaInputProjection {
    Equirectangular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanoramaOutputProjection {
    Rectilinear,
    StereographicLittlePlanet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanoramaProjectionSpec {
    pub input: PanoramaInputProjection,
    pub output: PanoramaOutputProjection,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub roll_deg: f32,
    pub field_of_view_deg: f32,
    pub zoom: f32,
    pub seam_offset_deg: f32,
    pub seam_wrap: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PanoramaProjectionError {
    #[error("panorama input dimensions must be non-zero")]
    InvalidInputDimensions,
    #[error("panorama input has {actual} pixels; expected {expected}")]
    InvalidInputBufferLength { expected: usize, actual: usize },
    #[error("panorama input contains a non-finite channel")]
    NonFiniteInput,
    #[error("panorama output dimensions are invalid")]
    InvalidOutputDimensions,
    #[error("panorama parameter `{0}` must be finite")]
    NonFiniteParameter(&'static str),
    #[error("rectilinear field of view must be greater than 1 degree and less than 179 degrees")]
    InvalidFieldOfView,
    #[error("stereographic zoom must be greater than zero")]
    InvalidZoom,
    #[error("CPU Slice 0 requires horizontal seam wrapping")]
    SeamWrapRequired,
    #[error("GPU panorama {axis} {actual} exceeds the device limit {max}")]
    GpuTextureDimensionExceeded {
        axis: &'static str,
        actual: u32,
        max: u32,
    },
    #[error("GPU panorama {role} transfer layout is not representable")]
    GpuTransferLayoutInvalid { role: &'static str },
    #[error("GPU panorama readback requires {bytes} bytes; device buffer limit is {max}")]
    GpuReadbackExceedsMaxBuffer { bytes: u64, max: u64 },
    #[error("GPU panorama host upload size {bytes} is not representable")]
    GpuHostAllocationInvalid { bytes: u64 },
}

pub fn project_panorama_cpu(
    input: &Image,
    output_size: (u32, u32),
    spec: &PanoramaProjectionSpec,
) -> Result<Image, PanoramaProjectionError> {
    validate(input, output_size, spec)?;

    let (width, height) = output_size;
    let pixel_count = (width * height) as usize;
    let mut pixels = Vec::with_capacity(pixel_count);
    let rotation = rotation_matrix(spec);
    let aspect = width as f64 / height as f64;

    for y in 0..height {
        for x in 0..width {
            let ray = match spec.output {
                PanoramaOutputProjection::Rectilinear => {
                    rectilinear_ray(x, y, output_size, spec.field_of_view_deg)
                }
                PanoramaOutputProjection::StereographicLittlePlanet => {
                    let plane_x =
                        (2.0 * (x as f64 + 0.5) / width as f64 - 1.0) * aspect / spec.zoom as f64;
                    let plane_up =
                        (1.0 - 2.0 * (y as f64 + 0.5) / height as f64) / spec.zoom as f64;
                    stereographic_ray(plane_x, plane_up)
                }
            };
            let world_ray = rotation * ray;
            let longitude = world_ray.x.atan2(world_ray.z);
            let latitude = world_ray.y.clamp(-1.0, 1.0).asin();
            let u = ((longitude + spec.seam_offset_deg.to_radians()) / TAU + 0.5).rem_euclid(1.0);
            let v = (0.5 - latitude / PI).clamp(0.0, 1.0);
            pixels.push(sample_equirectangular(input, u, v));
        }
    }

    debug_assert!(pixels.iter().flatten().all(|channel| channel.is_finite()));
    Ok(Image {
        width,
        height,
        pixels,
    })
}

pub(crate) fn validate(
    input: &Image,
    output_size: (u32, u32),
    spec: &PanoramaProjectionSpec,
) -> Result<(), PanoramaProjectionError> {
    let Some(input_len) = input.width.checked_mul(input.height) else {
        return Err(PanoramaProjectionError::InvalidInputDimensions);
    };
    if input.width == 0 || input.height == 0 {
        return Err(PanoramaProjectionError::InvalidInputDimensions);
    }
    if input.pixels.len() != input_len as usize {
        return Err(PanoramaProjectionError::InvalidInputBufferLength {
            expected: input_len as usize,
            actual: input.pixels.len(),
        });
    }
    if !input
        .pixels
        .iter()
        .flatten()
        .all(|channel| channel.is_finite())
    {
        return Err(PanoramaProjectionError::NonFiniteInput);
    }

    let (width, height) = output_size;
    if width == 0 || height == 0 || width.checked_mul(height).is_none() {
        return Err(PanoramaProjectionError::InvalidOutputDimensions);
    }

    for (name, value) in [
        ("yaw_deg", spec.yaw_deg),
        ("pitch_deg", spec.pitch_deg),
        ("roll_deg", spec.roll_deg),
        ("field_of_view_deg", spec.field_of_view_deg),
        ("zoom", spec.zoom),
        ("seam_offset_deg", spec.seam_offset_deg),
    ] {
        if !value.is_finite() {
            return Err(PanoramaProjectionError::NonFiniteParameter(name));
        }
    }

    match spec.output {
        PanoramaOutputProjection::Rectilinear
            if !(1.0 < spec.field_of_view_deg && spec.field_of_view_deg < 179.0) =>
        {
            return Err(PanoramaProjectionError::InvalidFieldOfView);
        }
        PanoramaOutputProjection::StereographicLittlePlanet if spec.zoom <= 0.0 => {
            return Err(PanoramaProjectionError::InvalidZoom);
        }
        _ => {}
    }
    if !spec.seam_wrap {
        return Err(PanoramaProjectionError::SeamWrapRequired);
    }
    Ok(())
}

fn rectilinear_ray(x: u32, y: u32, size: (u32, u32), fov_deg: f32) -> Vec3 {
    let (width, height) = size;
    let half_width = (fov_deg.to_radians() * 0.5).tan();
    let aspect = width as f32 / height as f32;
    let camera_x = (2.0 * (x as f32 + 0.5) / width as f32 - 1.0) * half_width;
    let camera_y = (1.0 - 2.0 * (y as f32 + 0.5) / height as f32) * half_width / aspect;
    Vec3::new(camera_x, camera_y, 1.0).normalize()
}

// Inverse stereographic projection with the output plane tangent at the south
// pole: the center is nadir, radius one is the horizon, and infinity tends to
// the north pole.
fn stereographic_ray(plane_x: f64, plane_up: f64) -> Vec3 {
    let radius_squared = plane_x * plane_x + plane_up * plane_up;
    let denominator = 1.0 + radius_squared;
    Vec3::new(
        (2.0 * plane_x / denominator) as f32,
        ((radius_squared - 1.0) / denominator) as f32,
        (2.0 * plane_up / denominator) as f32,
    )
}

pub(crate) fn rotation_matrix(spec: &PanoramaProjectionSpec) -> Mat3 {
    Mat3::from_rotation_y(spec.yaw_deg.to_radians())
        * Mat3::from_rotation_x(spec.pitch_deg.to_radians())
        * Mat3::from_rotation_z(spec.roll_deg.to_radians())
}

#[cfg(test)]
fn rotate_ray(ray: Vec3, spec: &PanoramaProjectionSpec) -> Vec3 {
    rotation_matrix(spec) * ray
}

fn sample_equirectangular(input: &Image, u: f32, v: f32) -> [f32; 4] {
    let x = u * input.width as f32 - 0.5;
    let y = v * input.height as f32 - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let width = input.width as i64;
    let height = input.height as i64;

    let at = |sample_x: i64, sample_y: i64| {
        let wrapped_x = sample_x.rem_euclid(width) as usize;
        let clamped_y = sample_y.clamp(0, height - 1) as usize;
        input.pixels[clamped_y * input.width as usize + wrapped_x]
    };
    let p00 = at(x0, y0);
    let p10 = at(x0 + 1, y0);
    let p01 = at(x0, y0 + 1);
    let p11 = at(x0 + 1, y0 + 1);
    let mut output = [0.0; 4];
    for channel in 0..4 {
        let top = p00[channel] * (1.0 - fx) + p10[channel] * fx;
        let bottom = p01[channel] * (1.0 - fx) + p11[channel] * fx;
        output[channel] = top * (1.0 - fy) + bottom * fy;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-5;

    fn spec(output: PanoramaOutputProjection) -> PanoramaProjectionSpec {
        PanoramaProjectionSpec {
            input: PanoramaInputProjection::Equirectangular,
            output,
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
            field_of_view_deg: 90.0,
            zoom: 1.0,
            seam_offset_deg: 0.0,
            seam_wrap: true,
        }
    }

    fn image(width: u32, height: u32, pixels: Vec<[f32; 4]>) -> Image {
        Image {
            width,
            height,
            pixels,
        }
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).abs().max_element() <= EPSILON,
            "{actual:?} != {expected:?}"
        );
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4]) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= EPSILON,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn rectilinear_center_and_yaw_pitch_roll_follow_locked_axes() {
        let center = rectilinear_ray(0, 0, (1, 1), 90.0);
        assert_vec3_close(center, Vec3::Z);

        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.yaw_deg = 90.0;
        assert_vec3_close(rotate_ray(center, &projection), Vec3::X);

        projection.yaw_deg = 0.0;
        projection.pitch_deg = 90.0;
        assert_vec3_close(rotate_ray(center, &projection), -Vec3::Y);

        projection.pitch_deg = 0.0;
        projection.roll_deg = 90.0;
        let right = rectilinear_ray(2, 1, (3, 3), 90.0);
        let rolled = rotate_ray(right, &projection);
        assert!(rolled.y > 0.0);
        assert!(rolled.x.abs() <= EPSILON);
    }

    #[test]
    fn stereographic_center_horizon_and_rotated_poles_are_known() {
        assert_vec3_close(stereographic_ray(0.0, 0.0), -Vec3::Y);
        assert_vec3_close(stereographic_ray(0.0, 1.0), Vec3::Z);

        let source = image(1, 2, vec![[0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);
        let mut projection = spec(PanoramaOutputProjection::StereographicLittlePlanet);
        let south = project_panorama_cpu(&source, (1, 1), &projection).unwrap();
        assert_pixel_close(south.pixels[0], [1.0, 0.0, 0.0, 1.0]);

        projection.roll_deg = 180.0;
        let north = project_panorama_cpu(&source, (1, 1), &projection).unwrap();
        assert_pixel_close(north.pixels[0], [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn horizontal_seam_wraps_during_bilinear_sampling() {
        let source = image(
            4,
            1,
            vec![
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0],
            ],
        );
        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.yaw_deg = 180.0;
        let output = project_panorama_cpu(&source, (1, 1), &projection).unwrap();
        assert_pixel_close(output.pixels[0], [0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn latitude_clamps_to_top_and_bottom_rows() {
        let source = image(1, 2, vec![[0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);
        let mut projection = spec(PanoramaOutputProjection::Rectilinear);
        projection.pitch_deg = -90.0;
        assert_pixel_close(
            project_panorama_cpu(&source, (1, 1), &projection)
                .unwrap()
                .pixels[0],
            [0.0, 1.0, 0.0, 1.0],
        );
        projection.pitch_deg = 90.0;
        assert_pixel_close(
            project_panorama_cpu(&source, (1, 1), &projection)
                .unwrap()
                .pixels[0],
            [1.0, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    fn rejects_invalid_sizes_parameters_and_non_finite_input() {
        let source = image(1, 1, vec![[0.0; 4]]);
        let projection = spec(PanoramaOutputProjection::Rectilinear);
        assert_eq!(
            project_panorama_cpu(&source, (0, 1), &projection),
            Err(PanoramaProjectionError::InvalidOutputDimensions)
        );
        assert_eq!(
            project_panorama_cpu(&source, (u32::MAX, 2), &projection),
            Err(PanoramaProjectionError::InvalidOutputDimensions)
        );

        let malformed = image(2, 1, vec![[0.0; 4]]);
        assert_eq!(
            project_panorama_cpu(&malformed, (1, 1), &projection),
            Err(PanoramaProjectionError::InvalidInputBufferLength {
                expected: 2,
                actual: 1,
            })
        );

        for fov in [f32::NAN, 1.0, 179.0, f32::INFINITY] {
            let mut invalid = projection;
            invalid.field_of_view_deg = fov;
            assert!(project_panorama_cpu(&source, (1, 1), &invalid).is_err());
        }
        for (name, value) in [
            ("yaw_deg", f32::NAN),
            ("pitch_deg", f32::INFINITY),
            ("roll_deg", f32::NEG_INFINITY),
            ("seam_offset_deg", f32::NAN),
        ] {
            let mut invalid = projection;
            match name {
                "yaw_deg" => invalid.yaw_deg = value,
                "pitch_deg" => invalid.pitch_deg = value,
                "roll_deg" => invalid.roll_deg = value,
                _ => invalid.seam_offset_deg = value,
            }
            assert_eq!(
                project_panorama_cpu(&source, (1, 1), &invalid),
                Err(PanoramaProjectionError::NonFiniteParameter(name))
            );
        }

        let mut stereographic = spec(PanoramaOutputProjection::StereographicLittlePlanet);
        for zoom in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            stereographic.zoom = zoom;
            assert!(project_panorama_cpu(&source, (1, 1), &stereographic).is_err());
        }

        let non_finite = image(1, 1, vec![[f32::NAN, 0.0, 0.0, 1.0]]);
        assert_eq!(
            project_panorama_cpu(&non_finite, (1, 1), &projection),
            Err(PanoramaProjectionError::NonFiniteInput)
        );

        let mut no_wrap = projection;
        no_wrap.seam_wrap = false;
        assert_eq!(
            project_panorama_cpu(&source, (1, 1), &no_wrap),
            Err(PanoramaProjectionError::SeamWrapRequired)
        );
    }

    #[test]
    fn bilinear_sampling_interpolates_premultiplied_alpha_directly() {
        let source = image(2, 1, vec![[0.0; 4], [0.5, 0.0, 0.0, 0.5]]);
        let output = project_panorama_cpu(
            &source,
            (1, 1),
            &spec(PanoramaOutputProjection::Rectilinear),
        )
        .unwrap();
        assert_pixel_close(output.pixels[0], [0.25, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn identical_inputs_are_byte_identical_and_finite() {
        let source = image(
            2,
            2,
            vec![
                [0.1, 0.2, 0.3, 0.4],
                [0.5, 0.6, 0.7, 0.8],
                [0.2, 0.3, 0.4, 0.5],
                [0.6, 0.7, 0.8, 0.9],
            ],
        );
        let projection = spec(PanoramaOutputProjection::StereographicLittlePlanet);
        let first = project_panorama_cpu(&source, (7, 5), &projection).unwrap();
        let second = project_panorama_cpu(&source, (7, 5), &projection).unwrap();
        assert_eq!(first, second);
        assert!(first
            .pixels
            .iter()
            .flatten()
            .all(|channel| channel.is_finite()));
    }
}
