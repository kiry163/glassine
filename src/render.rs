//! Premultiplied-RGBA compositing primitives.
//!
//! Every buffer here is premultiplied RGBA with `stride == width * 4`, which is
//! what `UpdateLayeredWindow` and `tiny-skia` both expect.

/// Resets a whole buffer to fully transparent.
pub fn clear(buffer: &mut [u8]) {
    buffer.fill(0);
}

/// Source-over compositing of one premultiplied pixel onto another.
///
/// `src[c] + (dst[c] * (255 - src[3]) + 127) / 255`, per channel. The `+ 127`
/// rounds the division to nearest; truncating instead darkens every blended
/// glyph edge against the transparent background.
pub fn blend_src_over(dst: &mut [u8], src: [u32; 4]) {
    debug_assert!(dst.len() >= 4, "a pixel is four bytes");
    let inverse = 255 - src[3];
    for channel in 0..4 {
        let blended = src[channel] + (dst[channel] as u32 * inverse + 127) / 255;
        dst[channel] = blended.min(255) as u8;
    }
}

/// Stamps an 8-bit coverage mask as a solid colour.
///
/// `coverage * alpha / 255` gives the pixel's alpha, the colour is premultiplied
/// by it, and only then is it composited, so partially covered pixels stay
/// premultiplied. Anything outside `dst_width` x `dst_height` is clipped rather
/// than wrapped: a mask that starts left of the window must not reappear on the
/// right edge.
#[allow(clippy::too_many_arguments)]
pub fn blit_coverage(
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    x0: i32,
    y0: i32,
    mask_width: u32,
    mask_height: u32,
    mask: &[u8],
    color: (u8, u8, u8),
    alpha: u8,
) {
    debug_assert!(
        dst.len() >= dst_width as usize * dst_height as usize * 4,
        "destination buffer must hold dst_width * dst_height pixels"
    );
    debug_assert!(
        mask.len() >= mask_width as usize * mask_height as usize,
        "mask must hold mask_width * mask_height bytes"
    );

    let (red, green, blue) = (color.0 as u32, color.1 as u32, color.2 as u32);
    let alpha = alpha as u32;

    for mask_y in 0..mask_height {
        let y = y0 + mask_y as i32;
        if y < 0 || y >= dst_height as i32 {
            continue;
        }
        for mask_x in 0..mask_width {
            let x = x0 + mask_x as i32;
            if x < 0 || x >= dst_width as i32 {
                continue;
            }
            let coverage = mask[(mask_y * mask_width + mask_x) as usize] as u32;
            let pixel_alpha = (coverage * alpha + 127) / 255;
            if pixel_alpha == 0 {
                continue;
            }
            let offset = (y as usize * dst_width as usize + x as usize) * 4;
            blend_src_over(
                &mut dst[offset..offset + 4],
                [
                    (red * pixel_alpha + 127) / 255,
                    (green * pixel_alpha + 127) / 255,
                    (blue * pixel_alpha + 127) / 255,
                    pixel_alpha,
                ],
            );
        }
    }
}

/// Scales a whole buffer's alpha by window opacity, in percent.
///
/// Multiplying all four channels — not just alpha — is what keeps the buffer
/// premultiplied: scaling alpha alone would brighten every pixel against the
/// desktop. The division truncates, which cannot break that invariant.
pub fn apply_opacity(buffer: &mut [u8], opacity: u8) {
    if opacity == 100 {
        return;
    }
    let opacity = opacity as u32;
    for channel in buffer.iter_mut() {
        *channel = (*channel as u32 * opacity / 100) as u8;
    }
}

/// Swaps red and blue in place, converting RGBA to the BGRA that Win32 expects.
pub fn rgba_to_bgra_in_place(buffer: &mut [u8]) {
    debug_assert_eq!(buffer.len() % 4, 0, "buffer must be whole pixels");
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_source_over_in_premultiplied_space() {
        let mut dst = [255u8, 0, 0, 255];
        blend_src_over(&mut dst, [128, 128, 128, 128]);
        assert_eq!(dst, [255, 128, 128, 255]);
    }

    #[test]
    fn transparent_source_leaves_destination_untouched() {
        let mut dst = [10u8, 20, 30, 255];
        blend_src_over(&mut dst, [0, 0, 0, 0]);
        assert_eq!(dst, [10, 20, 30, 255]);
    }

    #[test]
    fn opaque_source_replaces_destination() {
        let mut dst = [10u8, 20, 30, 255];
        blend_src_over(&mut dst, [1, 2, 3, 255]);
        assert_eq!(dst, [1, 2, 3, 255]);
    }

    #[test]
    fn transparent_destination_takes_source_verbatim() {
        let mut dst = [0u8; 4];
        blend_src_over(&mut dst, [64, 32, 16, 64]);
        assert_eq!(dst, [64, 32, 16, 64]);
    }

    #[test]
    fn base_alpha_scales_coverage() {
        let mut dst = [0u8; 4];
        blit_coverage(&mut dst, 1, 1, 0, 0, 1, 1, &[255], (255, 255, 255), 128);
        assert_eq!(dst, [128, 128, 128, 128]);
    }

    #[test]
    fn full_coverage_produces_the_premultiplied_colour() {
        // Colour (255, 32, 32) at alpha 230 is the exact case measured on the spike.
        let mut dst = [0u8; 4];
        blit_coverage(&mut dst, 1, 1, 0, 0, 1, 1, &[255], (255, 32, 32), 230);
        assert_eq!(dst, [230, 29, 29, 230]);
    }

    #[test]
    fn composited_pixels_stay_premultiplied() {
        let mut dst = vec![0u8; 16];
        blit_coverage(&mut dst, 4, 1, 0, 0, 4, 1, &[0, 64, 128, 255], (255, 255, 255), 255);
        for px in dst.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
        assert_eq!(dst[3], 0);
        assert_eq!(dst[7], 64);
        assert_eq!(dst[15], 255);
    }

    #[test]
    fn coverage_outside_the_buffer_is_clipped_not_wrapped() {
        let mut dst = vec![0u8; 16];
        // A 4x1 mask placed at x = -1 and y = -1: only the last mask column and no
        // rows are inside. Nothing may touch bytes outside the buffer.
        blit_coverage(&mut dst, 4, 1, -1, -1, 4, 1, &[9, 9, 9, 9], (255, 255, 255), 255);
        assert_eq!(dst, vec![0u8; 16]);

        blit_coverage(&mut dst, 4, 1, 3, 0, 4, 1, &[9, 9, 9, 9], (255, 255, 255), 255);
        assert_eq!(dst[15], 9);
        assert_eq!(&dst[0..12], &[0u8; 12]);
    }

    #[test]
    fn opacity_scales_all_four_channels() {
        let mut buf = [230u8, 29, 29, 230];
        apply_opacity(&mut buf, 50);
        assert_eq!(buf, [115, 14, 14, 115]);
        let mut buf = [230u8, 29, 29, 230];
        apply_opacity(&mut buf, 100);
        assert_eq!(buf, [230, 29, 29, 230]);
    }

    #[test]
    fn opacity_scaling_keeps_pixels_premultiplied() {
        let mut buf = [230u8, 29, 29, 230, 0, 0, 0, 0];
        apply_opacity(&mut buf, 33);
        for px in buf.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "{px:?}");
        }
    }

    #[test]
    fn bgra_swap_touches_only_red_and_blue() {
        let mut buf = [1u8, 2, 3, 4];
        rgba_to_bgra_in_place(&mut buf);
        assert_eq!(buf, [3, 2, 1, 4]);
    }

    #[test]
    fn clear_writes_fully_transparent() {
        let mut buf = [9u8; 8];
        clear(&mut buf);
        assert_eq!(buf, [0u8; 8]);
    }
}
