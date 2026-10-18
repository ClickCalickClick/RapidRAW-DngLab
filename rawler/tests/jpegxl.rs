// Lossy (XYB) JPEG XL tiles: decompressors/jpegxl.rs restores the declared
// primaries instead of letting jxl-oxide gamut-map or clip in linear sRGB.

use jxl_oxide::JxlImage;
use rawler::decompressors::jpegxl::JpegXLDecompressor;
use rawler::decompressors::Decompressor;

// 64x64 16-bit tiles of four 16-row ramps (grey, blue, red, green camera
// RGB), stored as 10-bit square-root codes like DxO's, lossy (XYB) at
// distance 0.3 and declared linear BT.2100 or linear sRGB. Made with libjxl
// 0.12 (imagecodecs); xyb_patches.u16le holds the encoder's input.
const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/testdata/jpegxl/");

fn input() -> Vec<u16> {
  std::fs::read(format!("{DIR}xyb_patches.u16le")).unwrap().chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

fn decode(file: &str) -> Vec<u16> {
  let src = std::fs::read(format!("{DIR}{file}")).unwrap();
  let mut out = vec![0_u16; 64 * 64 * 3];
  JpegXLDecompressor::new(16).decompress(&src, 0, out.chunks_exact_mut(64 * 3), 64 * 3).unwrap();
  out
}

/// Mean |decoded - input| in codes over the right half of each ramp.
fn band_errors(decoded: &[u16], input: &[u16]) -> [f64; 4] {
  std::array::from_fn(|band| {
    let mut sum = 0.0;
    let mut n = 0;
    for row in 16 * band..16 * (band + 1) {
      for i in row * 192 + 96..(row + 1) * 192 {
        sum += (decoded[i] as f64 - input[i] as f64).abs();
        n += 1;
      }
    }
    sum / n as f64
  })
}

#[test]
fn xyb_bt2100_tile_keeps_colours_outside_srgb() {
  let input = input();
  let src = std::fs::read(format!("{DIR}xyb_bt2100_linear.jxl")).unwrap();
  let image = JxlImage::builder().read(src.as_slice()).unwrap();
  let mut default = vec![0_u16; 64 * 64 * 3];
  image.render_frame(0).unwrap().stream_no_alpha().write_to_buffer(&mut default);
  let before = band_errors(&default, &input);
  let after = band_errors(&decode("xyb_bt2100_linear.jxl"), &input);
  eprintln!("mean error per ramp (grey, blue, red, green), default render {before:?}, ours {after:?}");
  // the grey ramp is inside sRGB: unchanged, up to rounding
  assert!((after[0] - before[0]).abs() < 0.1, "{before:?} {after:?}");
  // the green ramp lies outside sRGB: the default render clipped it there
  assert!(before[3] > 3.0 * after[3], "{before:?} {after:?}");
  // and every ramp is within the codec's own loss
  assert!(after.iter().all(|e| *e < 3.0), "{after:?}");
}

#[test]
fn xyb_srgb_tile_matches_the_default_render() {
  let src = std::fs::read(format!("{DIR}xyb_srgb_linear.jxl")).unwrap();
  let image = JxlImage::builder().read(src.as_slice()).unwrap();
  let mut default = vec![0_u16; 64 * 64 * 3];
  image.render_frame(0).unwrap().stream_no_alpha().write_to_buffer(&mut default);
  let ours = decode("xyb_srgb_linear.jxl");
  let max = default.iter().zip(&ours).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
  assert!(max <= 1, "max difference {max} codes");
}
