// SPDX-License-Identifier: LGPL-2.1
// Copyright 2025 Daniel Vogelbacher <daniel@chaospixel.com>

use jxl_oxide::color::{ColourEncoding, ColourSpace, Primaries, TransferFunction, WhitePoint};
use jxl_oxide::image::BitDepth;
use jxl_oxide::{EnumColourEncoding, JxlImage, RenderingIntent};

use crate::decompressors::{Decompressor, LineIteratorMut};

pub struct JpegXLDecompressor {
  bps: u32,
}

impl JpegXLDecompressor {
  pub fn new(bps: u32) -> Self {
    Self { bps }
  }
}

impl<'a> Decompressor<'a, u16> for JpegXLDecompressor {
  fn decompress(&self, src: &[u8], mut skip_rows: usize, lines: impl LineIteratorMut<'a, u16>, line_width: usize) -> std::result::Result<(), String> {
    let image = JxlImage::builder()
      .read(src)
      .map_err(|err| format!("Failed to read JPEG-XL image: {:?}", err))?;
    if (9..=16).contains(&self.bps) {
      if let Some(to_declared) = xyb_to_declared(&image) {
        return decompress_xyb(image, to_declared, skip_rows, lines, line_width);
      }
    }
    if let Some(header) = image.frame_header(0) {
      if header.bit_depth.bits_per_sample() != 8 && header.bit_depth.bits_per_sample() != 16 {
        // jxl_oxide scales the pixels into full range of storage type.
        // If we get e.g. 12 bit compressed data, output is scaled to 16 bit.
        // This breaks blacklevel scaling. We need to scale back to given bps (from TIFF).
        unimplemented!("JPEG-XL bit-depth {} not supported yet", header.bit_depth.bits_per_sample());
      }
      //eprintln!("JPEG-XL Bit-Depth: {:?}", header.bit_depth);
    }
    let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;

    let mut stream = frame.stream_no_alpha();

    match self.bps {
      8 => {
        let mut tmp = vec![0_u8; line_width];

        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(&mut tmp);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(&mut tmp);
          assert_eq!(line.len(), written);
          for (p, x) in line.iter_mut().zip(tmp.iter()) {
            *p = *x as u16; // Only change storage format, you MUST NOT scale up!
          }
        }
      }
      9..=16 => {
        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(line);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(line);
          assert_eq!(line.len(), written);
        }
      }
      _ => unimplemented!(),
    }

    /*
    let all_ch = frame.image_all_channels();

    let pixbuf = all_ch.buf();
    for (line, buf) in lines.zip(pixbuf.chunks_exact(line_width).skip(skip_rows)) {
      for (p, f) in line.iter_mut().zip(buf.iter()) {
        //debug_assert!(*f <= (1.0 + f32::EPSILON));
        // *p = (f * u16::MAX as f32) as u16;
        // *p = *f as u16;
      }
    }
    */
    Ok(())
  }
}

type Matrix = [[f64; 3]; 3];

/// Lossy (XYB) tiles: jxl-oxide's default render converts to the declared
/// colour encoding by way of linear sRGB, and on the way it gamut-maps
/// (perceptual intent) or clips (relative) every sample outside sRGB. A
/// LinearRaw DNG's samples are camera RGB, not colours in the declared space,
/// so that desaturates them: DxO PureRAW 6 / XD3 files declare linear BT.2100
/// with a perceptual intent. When the declared encoding is linear RGB with a
/// D65 white point and no HDR tone mapping applies, decode to linear sRGB and
/// restore the declared primaries with a plain 3x3 matrix instead (as
/// LightCraft's crates/raw/src/jxl.rs does). Returns that matrix (identity
/// for sRGB primaries), or None to keep the default render.
fn xyb_to_declared(image: &JxlImage) -> Option<Matrix> {
  let header = image.image_header();
  let meta = &header.metadata;
  if !meta.xyb_encoded || meta.grayscale() || meta.tone_mapping.intensity_target > 255.0 {
    return None;
  }
  if !matches!(meta.bit_depth, BitDepth::IntegerSample { bits_per_sample: 16 }) {
    return None;
  }
  let ColourEncoding::Enum(e) = &meta.colour_encoding else {
    return None;
  };
  if e.colour_space != ColourSpace::Rgb || e.tf != TransferFunction::Linear || e.white_point != WhitePoint::D65 {
    return None;
  }
  const SRGB: [[f64; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];
  let primaries = match e.primaries {
    Primaries::Srgb => SRGB,
    Primaries::Bt2100 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
    Primaries::P3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
    Primaries::Custom { red, green, blue } => [red, green, blue].map(|c| c.as_float().map(f64::from)),
  };
  if primaries.iter().any(|p| p[1] <= 0.0) {
    return None;
  }
  let m = mul(inverse(rgb_to_xyz(primaries)), rgb_to_xyz(SRGB));
  m.iter().flatten().all(|v| v.is_finite()).then_some(m)
}

fn decompress_xyb<'a>(
  mut image: JxlImage,
  m: Matrix,
  skip_rows: usize,
  lines: impl LineIteratorMut<'a, u16>,
  line_width: usize,
) -> std::result::Result<(), String> {
  image.request_color_encoding(EnumColourEncoding::srgb_linear(RenderingIntent::Relative));
  let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;
  let mut stream = frame.stream_no_alpha();
  if stream.channels() != 3 || line_width != stream.width() as usize * 3 {
    return Err(format!(
      "JPEG-XL tile is {}x{}, line is {} samples",
      stream.width(),
      stream.channels(),
      line_width
    ));
  }
  let mut buf = vec![0_f32; line_width * stream.height() as usize];
  stream.write_to_buffer(&mut buf);
  let m = m.map(|r| r.map(|v| v as f32));
  for (line, row) in lines.skip(skip_rows).zip(buf.chunks_exact(line_width).skip(skip_rows)) {
    for (out, px) in line.chunks_exact_mut(3).zip(row.chunks_exact(3)) {
      for (o, mr) in out.iter_mut().zip(m.iter()) {
        let v = mr[0] * px[0] + mr[1] * px[1] + mr[2] * px[2];
        // the rounding jxl-oxide's own u16 output uses
        *o = (v * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16;
      }
    }
  }
  Ok(())
}

fn inverse(m: Matrix) -> Matrix {
  let c = |i: usize, j: usize| m[(i + 1) % 3][(j + 1) % 3] * m[(i + 2) % 3][(j + 2) % 3] - m[(i + 1) % 3][(j + 2) % 3] * m[(i + 2) % 3][(j + 1) % 3];
  let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
  std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det))
}

fn mul(a: Matrix, b: Matrix) -> Matrix {
  std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// Linear RGB -> XYZ for primaries `p` (xy) and a D65 white point.
fn rgb_to_xyz(p: [[f64; 2]; 3]) -> Matrix {
  let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
  let cols = [xyz(p[0]), xyz(p[1]), xyz(p[2])];
  let prim = [[cols[0][0], cols[1][0], cols[2][0]], [1.0; 3], [cols[0][2], cols[1][2], cols[2][2]]];
  let w = xyz([0.3127, 0.3290]);
  let inv = inverse(prim);
  let s: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| inv[i][k] * w[k]).sum());
  std::array::from_fn(|i| std::array::from_fn(|j| prim[i][j] * s[j]))
}
