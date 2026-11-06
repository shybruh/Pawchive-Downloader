//! Windows' own thumbnails (what File Explorer shows), through IShellItemImageFactory. Covers video
//! and any picture format Windows has a codec for (HEIC, RAW…), without ffmpeg.

use image::RgbaImage;
use std::path::Path;
use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY,
};

/// A real thumbnail of the file fitting `size`, or None (no codec, no thumbnail).
pub fn thumbnail(path: &Path, size: u32) -> Option<RgbaImage> {
    unsafe {
        // The media server's worker threads may not have COM set up yet
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = fetch(path, size);
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

fn fetch(path: &Path, size: u32) -> Option<RgbaImage> {
    // Windows API calls: COM, GDI handles
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None).ok()?;
        let side = size as i32;
        // A real thumbnail or nothing (never the generic file icon)
        let hbitmap = factory.GetImage(SIZE { cx: side, cy: side }, SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY).ok()?;
        let object = HGDIOBJ(hbitmap.0);

        let read = || -> Option<RgbaImage> {
            let mut bm = BITMAP::default();
            if GetObjectW(object, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
            if w <= 0 || h <= 0 {
                return None;
            }
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let hdc = GetDC(None);
            let rows = GetDIBits(hdc, hbitmap, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
            ReleaseDC(None, hdc);
            if rows != h {
                return None;
            }
            // BGRA → RGBA. Many shell thumbnails leave alpha at 0 although they're opaque.
            let opaque = !buf.chunks_exact(4).any(|p| p[3] != 0);
            for p in buf.chunks_exact_mut(4) {
                p.swap(0, 2);
                if opaque {
                    p[3] = 255;
                }
            }
            RgbaImage::from_raw(w as u32, h as u32, buf)
        };
        let img = read();
        let _ = DeleteObject(object);
        img
    }
}
