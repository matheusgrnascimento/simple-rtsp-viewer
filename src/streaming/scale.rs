//! Output size of the decoded frames (pure logic).

use crate::model::DisplaySize;

/// Size at which a `source_width`x`source_height` frame should be converted to
/// RGBA before going to the UI: the largest size with the same aspect ratio
/// that fits in `display`, never enlarging the source (if the tile is larger
/// than the video, the GPU scales it up when drawing).
///
/// The width comes out as a multiple of 4 (from 4 pixels up): each RGBA row is
/// then a multiple of 16 bytes, the alignment swscale needs to use its
/// vectorized routines. Without it, swscale falls back to a slower path and
/// prints a warning to the console. A difference of up to 3 pixels in width is
/// not visible.
///
/// `None` or zero sizes keep the source resolution (with the width aligned).
pub fn output_size(
    source_width: u32,
    source_height: u32,
    display: Option<DisplaySize>,
) -> (u32, u32) {
    let source = (align_width(source_width), source_height);
    let Some(display) = display else {
        return source;
    };
    if source_width == 0 || source_height == 0 || display.width == 0 || display.height == 0 {
        return source;
    }
    if source_width <= display.width && source_height <= display.height {
        return source;
    }

    let scale = (f64::from(display.width) / f64::from(source_width))
        .min(f64::from(display.height) / f64::from(source_height));
    let width = (f64::from(source_width) * scale).round() as u32;
    let height = (f64::from(source_height) * scale).round() as u32;
    (
        align_width(width.clamp(1, source_width)),
        height.clamp(1, source_height),
    )
}

/// Rounds the width down to a multiple of 4, without going below 4 (and without
/// touching widths smaller than that).
fn align_width(width: u32) -> u32 {
    if width < 4 {
        width
    } else {
        width / 4 * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn display(width: u32, height: u32) -> Option<DisplaySize> {
        Some(DisplaySize { width, height })
    }

    #[test]
    fn main_stream_is_reduced_to_fit_the_block() {
        // 2560x1440 (16:9) in a 640x480 tile: limited by the width.
        assert_eq!(output_size(2560, 1440, display(640, 480)), (640, 360));
        // Same video in a tall, narrow tile: limited by the height.
        assert_eq!(output_size(2560, 1440, display(1000, 270)), (480, 270));
    }

    #[test]
    fn video_smaller_than_the_block_is_never_enlarged() {
        assert_eq!(output_size(640, 360, display(1920, 1080)), (640, 360));
        assert_eq!(output_size(640, 360, display(640, 360)), (640, 360));
    }

    #[test]
    fn width_is_aligned_to_four_pixels() {
        // 1000x562 (16:9) limited to a width of 637 -> 637 becomes 636.
        assert_eq!(output_size(1000, 562, display(637, 1000)), (636, 358));
        // Source with an odd width and no scaling: only the width is aligned.
        assert_eq!(output_size(1281, 720, None), (1280, 720));
    }

    #[test]
    fn unknown_or_empty_display_keeps_source_size() {
        assert_eq!(output_size(2560, 1440, None), (2560, 1440));
        assert_eq!(output_size(2560, 1440, display(0, 480)), (2560, 1440));
        assert_eq!(output_size(2560, 1440, display(640, 0)), (2560, 1440));
        assert_eq!(output_size(0, 1440, display(640, 480)), (0, 1440));
    }

    proptest! {
        // The scaled frame fits in the tile and in the source, keeps the aspect
        // ratio (up to rounding to whole pixels) and fills the tile in at least
        // one dimension when the source is larger than it.
        #[test]
        fn property_output_size(
            source_width in 160u32..7680,
            source_height in 160u32..7680,
            display_width in 64u32..4000,
            display_height in 64u32..4000,
        ) {
            let (width, height) =
                output_size(source_width, source_height, display(display_width, display_height));

            prop_assert!(width >= 1 && height >= 1);
            prop_assert!(width <= source_width && height <= source_height);
            prop_assert_eq!(width % 4, 0);

            let fits = source_width <= display_width && source_height <= display_height;
            if fits {
                prop_assert_eq!((width, height), (source_width / 4 * 4, source_height));
            } else {
                prop_assert!(width <= display_width && height <= display_height);
                // The width may have lost up to 3 pixels to the alignment.
                prop_assert!(width + 3 >= display_width || height == display_height);
            }

            // |w/h - sw/sh| expressed without division: the height is off by at
            // most 1 pixel (rounding) and the width by up to 4 (rounding plus
            // alignment).
            let cross_error = (u64::from(width) * u64::from(source_height))
                .abs_diff(u64::from(height) * u64::from(source_width));
            prop_assert!(
                cross_error <= 4 * u64::from(source_height) + u64::from(source_width)
            );
        }
    }
}

/// Manual benchmark of the swscale filters, used to pick the one the decoder
/// uses: cost per frame and how sharp a hard edge stays after downscaling.
/// Run with:
/// `cargo test --release scaler_benchmark -- --ignored --nocapture`
#[cfg(test)]
mod scaler_benchmark {
    use std::time::Instant;

    use ffmpeg_next::format::Pixel;
    use ffmpeg_next::software::scaling::{Context, Flags};
    use ffmpeg_next::util::frame::video::Video;

    /// YUV420P frame with a hard vertical edge (black to white) at 1/3 of the
    /// width, like the border of a bright object.
    fn edge_frame(width: u32, height: u32) -> Video {
        let mut frame = Video::new(Pixel::YUV420P, width, height);
        let stride = frame.stride(0);
        let data = frame.data_mut(0);
        for y in 0..height as usize {
            for x in 0..width as usize {
                data[y * stride + x] = if x < width as usize / 3 { 16 } else { 235 };
            }
        }
        for plane in 1..3 {
            let stride = frame.stride(plane);
            let data = frame.data_mut(plane);
            for y in 0..height as usize / 2 {
                data[y * stride..y * stride + width as usize / 2].fill(128);
            }
        }
        frame
    }

    /// YUV420P frame of vertical stripes `period` pixels wide (half black,
    /// half white): fine detail, like small text or a fence.
    fn stripe_frame(width: u32, height: u32, period: usize) -> Video {
        let mut frame = Video::new(Pixel::YUV420P, width, height);
        let stride = frame.stride(0);
        let data = frame.data_mut(0);
        for y in 0..height as usize {
            for x in 0..width as usize {
                data[y * stride + x] = if x % period < period / 2 { 235 } else { 16 };
            }
        }
        for plane in 1..3 {
            let stride = frame.stride(plane);
            let data = frame.data_mut(plane);
            for y in 0..height as usize / 2 {
                data[y * stride..y * stride + width as usize / 2].fill(128);
            }
        }
        frame
    }

    /// Contrast kept on the middle row (0 to 1): the spread between the 5th
    /// and 95th percentile of the red channel. Higher means sharper detail.
    fn kept_contrast(rgba: &Video) -> f32 {
        let stride = rgba.stride(0);
        let row = &rgba.data(0)[(rgba.height() as usize / 2) * stride..];
        let mut red: Vec<u8> = (0..rgba.width() as usize).map(|x| row[x * 4]).collect();
        red.sort_unstable();
        let low = red[red.len() / 20];
        let high = red[red.len() - 1 - red.len() / 20];
        f32::from(high - low) / 255.0
    }

    /// (width of the black-to-white transition in output pixels, overshoot in
    /// percent of the edge height) measured on the middle row.
    fn edge_quality(rgba: &Video) -> (usize, f32) {
        let stride = rgba.stride(0);
        let row = &rgba.data(0)[(rgba.height() as usize / 2) * stride..];
        let red: Vec<f32> = (0..rgba.width() as usize)
            .map(|x| f32::from(row[x * 4]))
            .collect();
        let (low, high) = (red[2], red[red.len() - 3]);
        let span = high - low;
        let transition = red
            .iter()
            .filter(|value| **value > low + 0.1 * span && **value < high - 0.1 * span)
            .count();
        let overshoot = red.iter().fold(0.0_f32, |worst, value| {
            worst.max((value - high).max(low - value))
        });
        (transition, overshoot / span * 100.0)
    }

    #[test]
    #[ignore = "manual benchmark"]
    fn scaler_benchmark() {
        let scenarios = [
            (
                "2560x1440 -> 1920x1080 (enlarged main stream)",
                2560,
                1440,
                1920,
                1080,
            ),
            ("2560x1440 -> 1280x720", 2560, 1440, 1280, 720),
            (
                "640x360 -> 600x336 (grid tile from the substream)",
                640,
                360,
                600,
                336,
            ),
        ];
        let filters: [(&str, Flags); 6] = [
            ("BILINEAR (current)", Flags::BILINEAR),
            ("BICUBIC", Flags::BICUBIC),
            ("AREA", Flags::AREA),
            ("LANCZOS", Flags::LANCZOS),
            (
                "BICUBIC+ACCURATE_RND+FULL_CHR_H_INT",
                Flags::BICUBIC | Flags::ACCURATE_RND | Flags::FULL_CHR_H_INT,
            ),
            (
                "LANCZOS+ACCURATE_RND+FULL_CHR_H_INT",
                Flags::LANCZOS | Flags::ACCURATE_RND | Flags::FULL_CHR_H_INT,
            ),
        ];

        for (title, src_w, src_h, out_w, out_h) in scenarios {
            let source = edge_frame(src_w, src_h);
            let stripes = stripe_frame(src_w, src_h, 6);
            println!("\n{title}");
            println!(
                "  {:<40} {:>9} {:>9} {:>14}",
                "filter", "ms/frame", "overshoot", "fine detail"
            );
            for (name, flags) in &filters {
                let mut scaler = Context::get(
                    Pixel::YUV420P,
                    src_w,
                    src_h,
                    Pixel::RGBA,
                    out_w,
                    out_h,
                    *flags,
                )
                .expect("scaler");
                let mut out = Video::empty();
                for _ in 0..5 {
                    scaler.run(&source, &mut out).expect("warm-up");
                }
                let runs = 40;
                let started = Instant::now();
                for _ in 0..runs {
                    scaler.run(&source, &mut out).expect("run");
                }
                let millis = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
                let (_, overshoot) = edge_quality(&out);
                let mut detail_out = Video::empty();
                scaler.run(&stripes, &mut detail_out).expect("stripes");
                let detail = kept_contrast(&detail_out) * 100.0;
                println!("  {name:<40} {millis:>9.2} {overshoot:>8.1}% {detail:>13.0}%");
            }
        }
    }
}
