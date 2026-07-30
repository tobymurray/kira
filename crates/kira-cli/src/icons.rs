//! Deriving the site icons from the source artwork.
//!
//! Replaces a shell script that shelled out to `ImageMagick`, so the only
//! requirement is the toolchain.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, Rgba, RgbaImage, imageops};

/// The artwork this crop was measured against.
const EXPECTED: (u32, u32) = (1408, 768);

/// Measured bounding box of the mark itself.
///
/// Deliberately not a `-trim`-style autocrop: the source render carries a
/// generator watermark in the bottom-right corner, and trimming the whole image
/// yields 828x508, which would bake that watermark into every icon.
const CROP: (u32, u32, u32, u32) = (484, 164, 440, 440);

/// Background sampled from the source.
///
/// Kept opaque on purpose: the mark is white and cyan, so a transparent version
/// would vanish against a light background.
const BACKGROUND: Rgba<u8> = Rgba([0x08, 0x08, 0x08, 0xFF]);

/// Side of the master tile, and the size the mark is inset to within it.
const MASTER: u32 = 512;
const INSET: u32 = 424;

/// Sizes emitted as standalone PNGs.
const SIZES: [u32; 5] = [192, 180, 48, 32, 16];
/// Sizes packed into `favicon.ico`.
const ICO_SIZES: [u32; 3] = [48, 32, 16];
/// Link-preview card, at the ratio scrapers expect.
const CARD: (u32, u32, u32) = (1200, 630, 360);

/// Centre `source` on an opaque canvas of `size` x `size`.
fn tile(source: &DynamicImage, width: u32, height: u32) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(width, height, BACKGROUND);
    let x = i64::from(width - source.width()) / 2;
    let y = i64::from(height - source.height()) / 2;
    imageops::overlay(&mut canvas, &source.to_rgba8(), x, y);
    canvas
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<()> {
    image
        .save_with_format(path, image::ImageFormat::Png)
        .with_context(|| format!("writing {}", path.display()))
}

/// Pack PNGs into an ICO. PNG-compressed entries are valid and widely supported.
fn write_ico(images: &[(u32, Vec<u8>)], path: &Path) -> Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(u16::try_from(images.len())?).to_le_bytes());

    let header_len = 6 + 16 * images.len();
    let mut offset = u32::try_from(header_len)?;
    for (size, data) in images {
        // 0 means 256 in this field; every size here is smaller than that.
        out.push(u8::try_from(*size)?);
        out.push(u8::try_from(*size)?);
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(u32::try_from(data.len())?).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += u32::try_from(data.len())?;
    }
    for (_, data) in images {
        out.extend_from_slice(data);
    }

    let mut file = fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    file.write_all(&out)?;
    Ok(())
}

fn png_bytes(image: &RgbaImage) -> Result<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .context("encoding PNG")?;
    Ok(buffer.into_inner())
}

/// Regenerate every icon under `out`.
///
/// # Errors
/// Fails if the artwork is missing, or is not the size the crop was measured
/// against — better than silently cropping the wrong region.
pub(crate) fn run(src: &Path, out: &Path) -> Result<()> {
    let source = image::open(src).with_context(|| format!("reading {}", src.display()))?;
    if (source.width(), source.height()) != EXPECTED {
        bail!(
            "{} is {}x{}, expected {}x{}. The crop geometry was measured against \
             the original; re-measure the mark's bounding box before trusting \
             this with new artwork.",
            src.display(),
            source.width(),
            source.height(),
            EXPECTED.0,
            EXPECTED.1,
        );
    }

    let img_dir = out.join("img");
    fs::create_dir_all(&img_dir)?;

    // Master tile: the mark inset inside a square so it has breathing room at
    // small sizes, on its own background so it reads on any browser chrome.
    let cropped = source.crop_imm(CROP.0, CROP.1, CROP.2, CROP.3);
    let inset = cropped.resize(INSET, INSET, imageops::FilterType::Lanczos3);
    let master = tile(&inset, MASTER, MASTER);
    save_png(&master, &img_dir.join("mark-512.png"))?;

    let mut written = vec!["img/mark-512.png".to_owned()];
    let scaled = |size: u32| {
        DynamicImage::ImageRgba8(master.clone())
            .resize(size, size, imageops::FilterType::Lanczos3)
            .to_rgba8()
    };

    for size in SIZES {
        let path = img_dir.join(format!("mark-{size}.png"));
        save_png(&scaled(size), &path)?;
        written.push(format!("img/mark-{size}.png"));
    }

    let ico: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&size| Ok((size, png_bytes(&scaled(size))?)))
        .collect::<Result<_>>()?;
    write_ico(&ico, &out.join("favicon.ico"))?;

    // Composed on a fresh canvas rather than cropped from the source, so the
    // watermark cannot sneak in.
    let card_mark = DynamicImage::ImageRgba8(master.clone()).resize(
        CARD.2,
        CARD.2,
        imageops::FilterType::Lanczos3,
    );
    let card = tile(&card_mark, CARD.0, CARD.1);
    save_png(&card, &img_dir.join("og-image.png"))?;
    written.push("img/og-image.png".to_owned());
    written.push("favicon.ico".to_owned());

    println!("Wrote:");
    for path in written {
        println!("  {}/{path}", out.display());
    }
    Ok(())
}
