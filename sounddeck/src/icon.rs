use std::io::Cursor;

/// Only 8-bit PNGs are handled: the RGBA conversion below assumes one byte per
/// channel, so a 16-bit image would decode with a wrong stride.
const SUPPORTED_BIT_DEPTH: png::BitDepth = png::BitDepth::Eight;

/// Decode the embedded PNG icon to RGBA8 pixels.
pub fn decode_png(bytes: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let depth = reader.info().bit_depth;
    if depth != SUPPORTED_BIT_DEPTH {
        anyhow::bail!("icon must be 8-bit per channel (found {depth:?})");
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow::anyhow!("icon is too large"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for v in buf {
                out.extend_from_slice(&[v, v, v, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        png::ColorType::Indexed => {
            anyhow::bail!("indexed PNG icons are not supported");
        }
    };

    Ok((rgba, info.width, info.height))
}

/// Cheap box-filter downscale, used to create the 32x32 tray icon.
pub fn resize_rgba(src: &[u8], sw: u32, sh: u32, dst: u32) -> Vec<u8> {
    if sw == 0 || sh == 0 || dst == 0 {
        return Vec::new();
    }
    if sw == dst && sh == dst {
        return src.to_vec();
    }
    let mut out = vec![0u8; (dst * dst * 4) as usize];
    for y in 0..dst {
        for x in 0..dst {
            let x0 = x * sw / dst;
            let x1 = ((x + 1) * sw / dst).max(x0 + 1).min(sw);
            let y0 = y * sh / dst;
            let y1 = ((y + 1) * sh / dst).max(y0 + 1).min(sh);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * sw + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * dst + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / n.max(1)) as u8;
            }
        }
    }
    out
}

#[cfg(windows)]
pub fn hicon_from_rgba(
    rgba: &[u8],
    size: u32,
) -> windows::core::Result<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Foundation::E_INVALIDARG;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
        HBITMAP, HDC, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

    // Validate before touching GDI: a short buffer would otherwise read past the
    // end of `rgba` while filling the DIB section.
    let bytes = (size as usize)
        .checked_mul(size as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0 && *bytes <= rgba.len())
        .ok_or_else(|| {
            windows::core::Error::new(
                E_INVALIDARG,
                format!(
                    "rgba buffer of {} bytes cannot hold a {size}x{size} icon",
                    rgba.len()
                ),
            )
        })?;

    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            HDC::default(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE::default(),
            0,
        )?;

        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, bytes);
        for i in 0..bytes / 4 {
            let si = i * 4;
            pixels[si] = rgba[si + 2]; // B
            pixels[si + 1] = rgba[si + 1]; // G
            pixels[si + 2] = rgba[si]; // R
            pixels[si + 3] = rgba[si + 3]; // A
        }

        // 1bpp AND mask, all zero -> fully opaque; the alpha channel does the work.
        let mask_stride = (size.div_ceil(32) * 4) as usize;
        let mask_bytes = vec![0u8; mask_stride * size as usize];
        let mask: HBITMAP = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            Some(mask_bytes.as_ptr() as *const core::ffi::c_void),
        );

        let info = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon: HICON = CreateIconIndirect(&info)?;

        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
        Ok(icon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_png(
        width: u32,
        height: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(Cursor::new(&mut out), width, height);
            encoder.set_color(color);
            encoder.set_depth(depth);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn png_should_be_rejected_when_depth_is_not_eight() {
        let data = vec![0u8; 2 * 2 * 2];
        let bytes = encode_png(
            2,
            2,
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen,
            &data,
        );
        assert!(decode_png(&bytes).is_err());
    }

    #[test]
    fn bundled_icon_should_decode_as_rgba8() {
        let (rgba, width, height) = decode_png(include_bytes!("../assets/icon.png")).unwrap();
        assert_eq!(width, height);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
    }

    #[cfg(windows)]
    #[test]
    fn hicon_should_be_rejected_when_buffer_is_short() {
        assert!(hicon_from_rgba(&[0u8; 4], 2).is_err());
        assert!(hicon_from_rgba(&[], 2).is_err());
        assert!(hicon_from_rgba(&[0u8; 16], 0).is_err());
    }
}
