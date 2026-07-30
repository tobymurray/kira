//! Icons embedded in a `.uapp`, stored as ABGR2222: one byte per pixel, two
//! bits per channel, packed `(a << 6) | (b << 4) | (g << 2) | r`.

/// Failure to decode an icon field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Icons are square, which `app_merging.py` enforces at build time.
    #[error("icon of {len} bytes is not square")]
    NotSquare {
        /// Length of the field.
        len: usize,
    },
}

/// A decoded icon as 8-bit RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels, always equal to the width.
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub pixels: Vec<u8>,
}

/// Whether every pixel is fully transparent, i.e. there is no image here.
///
/// A declared icon length is not evidence of an icon. Glance apps built with
/// icons off still carry correctly *sized* fields: `convert_icon_or_zeros()` in
/// `app_merging.py` zero-fills them rather than writing a zero length, and all
/// six Glance apps in `apps-v1.3.0` are like that.
#[must_use]
pub fn is_blank(field: &[u8]) -> bool {
    field.iter().all(|px| px >> 6 == 0)
}

/// Decode an ABGR2222 field to RGBA.
///
/// # Errors
/// [`Error::NotSquare`] if the length is not a perfect square.
pub fn decode(field: &[u8]) -> Result<Rgba, Error> {
    let side = field.len().isqrt();
    if side * side != field.len() {
        return Err(Error::NotSquare { len: field.len() });
    }
    let side = side as u32;

    let mut pixels = Vec::with_capacity(field.len() * 4);
    for &px in field {
        // Two bits per channel, scaled 0..3 -> 0..255.
        pixels.extend_from_slice(&[
            (px & 0b11) * 85,
            ((px >> 2) & 0b11) * 85,
            ((px >> 4) & 0b11) * 85,
            ((px >> 6) & 0b11) * 85,
        ]);
    }

    Ok(Rgba {
        width: side,
        height: side,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uapp::{NORMAL_ICON_LEN, SMALL_ICON_LEN};

    #[test]
    fn decodes_two_bit_channels_to_rgba() {
        // 0xFF = every channel 3 -> opaque white; 0xC0 = alpha 3, colours 0.
        let icon = decode(&[0xFF, 0xC0, 0xC0, 0xFF]).unwrap();
        assert_eq!((icon.width, icon.height), (2, 2));
        assert_eq!(&icon.pixels[..4], &[255, 255, 255, 255]);
        assert_eq!(&icon.pixels[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn rejects_a_non_square_field() {
        assert_eq!(decode(&[0; 5]), Err(Error::NotSquare { len: 5 }));
    }

    #[test]
    fn a_zero_filled_field_is_no_icon_at_all() {
        assert!(is_blank(&vec![0; NORMAL_ICON_LEN]));
        assert!(is_blank(&vec![0; SMALL_ICON_LEN]));
    }

    #[test]
    fn any_opaque_pixel_makes_it_visible() {
        let mut icon = vec![0u8; NORMAL_ICON_LEN];
        icon[1234] = 0x40; // alpha 1, colours 0
        assert!(!is_blank(&icon));
    }

    #[test]
    fn colour_bits_alone_do_not_make_it_visible() {
        // Fully transparent pixels still render as nothing, whatever their RGB.
        assert!(is_blank(&vec![0x3F; NORMAL_ICON_LEN]));
    }

    #[test]
    fn real_icon_sizes_decode_to_expected_dimensions() {
        assert_eq!(decode(&vec![0; NORMAL_ICON_LEN]).unwrap().width, 60);
        assert_eq!(decode(&vec![0; SMALL_ICON_LEN]).unwrap().width, 30);
    }
}
