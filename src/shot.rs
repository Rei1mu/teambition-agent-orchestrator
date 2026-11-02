//! 截图。给 agent 用：一条命令出 png，不用自己拼 PowerShell 脚本。
//!
//! 为什么放在 exe 里而不是写成 skill 里的 PowerShell 脚本：
//! agent 每次要截图都得先找脚本、拼参数、处理编码，token 和时间都浪费在找路上。
//! `tba shot --out x.png` 一行就完事，skill 里只写这一行。
//!
//! 截图方式：
//!   全屏   BitBlt(GetDC(NULL))，带 CAPTUREBLT 能截到分层窗口
//!   窗口   PrintWindow(PW_RENDERFULLCONTENT) 优先，失败回落 BitBlt(GetWindowDC)
//! 两条路都不需要把窗口提到前台，后台窗口也能截。
//!
//! 编码：png crate + 自适应行滤波 + fdeflate（`Compression::Fast`）。
//! 不透明截图自动丢掉 alpha 通道（存 RGB），白捡 25% 体积且完全无损。
//! 实测 1920x1080 桌面：无压缩 8.0 MB -> fast 约 0.3–0.8 MB。

use std::path::Path;

#[cfg(windows)]
pub fn capture(
    out: &str,
    title: Option<&str>,
    hwnd: Option<isize>,
    level: &str,
) -> Result<String, String> {
    let (w, h, rgba, what) = win::grab(title, hwnd)?;
    let raw = w as usize * h as usize * 4;
    let png = png_encode(w as u32, h as u32, &rgba, level)?;
    let p = Path::new(out);
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::write(p, &png).map_err(|e| format!("写文件失败 {}: {e}", p.display()))?;
    let ratio = if png.is_empty() {
        0.0
    } else {
        raw as f64 / png.len() as f64
    };
    Ok(format!(
        "已截图: {} ({}x{}, {:.0} KB, 压缩 {:.1}x, level={})\n{}",
        p.display(),
        w,
        h,
        png.len() as f64 / 1024.0,
        ratio,
        level,
        what
    ))
}

#[cfg(not(windows))]
pub fn capture(
    _out: &str,
    _title: Option<&str>,
    _hwnd: Option<isize>,
    _level: &str,
) -> Result<String, String> {
    Err("截图只在 Windows 上可用".to_string())
}

// ---- PNG 编码 ----
//
// 之前是手写「deflate stored 块」（完全不压），全屏 8 MB。agent 每次读图都吃 token，
// 体积直接等于成本，所以换成真压缩。
//
// 三条无损优化，按收益排：
//   1. 不透明 -> 存 RGB 而不是 RGBA，少 25% 原始字节
//   2. 自适应行滤波（png crate 的 Adaptive）：截图大面积纯色，滤波后几乎全是 0
//   3. fdeflate：专门给 PNG 调的 deflate，比 zlib 快一个量级，压缩率还更好
//
// level 只影响第 3 步：fast = fdeflate（毫秒级）/ balanced / high = 通用 deflate（慢但更小）。
fn png_encode(w: u32, h: u32, rgba: &[u8], level: &str) -> Result<Vec<u8>, String> {
    // 截图基本都是不透明的（BitBlt 不保留 alpha）。真有透明像素才存 RGBA。
    let opaque = rgba.chunks_exact(4).all(|px| px[3] == 255);
    let (data, color) = if opaque {
        let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
        for px in rgba.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        (rgb, png::ColorType::Rgb)
    } else {
        (rgba.to_vec(), png::ColorType::Rgba)
    };

    let compression = match level {
        "fast" => png::Compression::Fast,
        "balanced" => png::Compression::Balanced,
        "high" => png::Compression::High,
        other => return Err(format!("未知压缩级别: {other}（可选 fast|balanced|high）")),
    };

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(compression);
        // 显式指定自适应滤波：Compression::Fast 默认也是它，但别指望默认值不变。
        enc.set_filter(png::Filter::Adaptive);
        let mut writer = enc.write_header().map_err(|e| format!("PNG 头写入失败: {e}"))?;
        writer
            .write_image_data(&data)
            .map_err(|e| format!("PNG 数据写入失败: {e}"))?;
        writer
            .finish()
            .map_err(|e| format!("PNG 收尾失败: {e}"))?;
    }
    Ok(out)
}

#[cfg(windows)]
mod win {
    use std::sync::Mutex;

    type Hwnd = isize;
    type Hdc = isize;
    type Hbitmap = isize;
    type Hgdiobj = isize;

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_pels: i32,
        y_pels: i32,
        clr_used: u32,
        clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        header: BitmapInfoHeader,
        colors: [u32; 1],
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetDC(h: Hwnd) -> Hdc;
        fn GetWindowDC(h: Hwnd) -> Hdc;
        fn ReleaseDC(h: Hwnd, dc: Hdc) -> i32;
        fn GetSystemMetrics(i: i32) -> i32;
        fn PrintWindow(h: Hwnd, dc: Hdc, flags: u32) -> i32;
        fn GetWindowRect(h: Hwnd, r: *mut Rect) -> i32;
        fn IsWindowVisible(h: Hwnd) -> i32;
        fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, l: isize) -> i32;
        fn GetWindowTextW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
        fn SetProcessDPIAware() -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: Hdc) -> Hdc;
        fn CreateCompatibleBitmap(dc: Hdc, w: i32, h: i32) -> Hbitmap;
        fn SelectObject(dc: Hdc, o: Hgdiobj) -> Hgdiobj;
        fn BitBlt(dst: Hdc, x: i32, y: i32, w: i32, h: i32, src: Hdc, sx: i32, sy: i32, rop: u32) -> i32;
        fn GetDIBits(
            dc: Hdc,
            bmp: Hbitmap,
            start: u32,
            lines: u32,
            bits: *mut u8,
            bi: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
        fn DeleteObject(o: Hgdiobj) -> i32;
        fn DeleteDC(dc: Hdc) -> i32;
    }

    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    const SRCCOPY: u32 = 0x00CC_0020;
    // 带上分层窗口（否则截不到带透明/GPU 合成的窗口）。
    const CAPTUREBLT: u32 = 0x4000_0000;
    const PW_RENDERFULLCONTENT: u32 = 2;
    const DIB_RGB_COLORS: u32 = 0;

    static NEEDLE: Mutex<String> = Mutex::new(String::new());
    static FOUND: Mutex<Option<Hwnd>> = Mutex::new(None);

    extern "system" fn enum_cb(h: Hwnd, _l: isize) -> i32 {
        let Ok(needle) = NEEDLE.lock() else {
            return 1;
        };
        if needle.is_empty() {
            return 1;
        }
        if unsafe { IsWindowVisible(h) } == 0 {
            return 1;
        }
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
        if n <= 0 {
            return 1;
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        if title.contains(needle.as_str()) {
            if let Ok(mut f) = FOUND.lock() {
                *f = Some(h);
            }
            return 0; // 找到就停
        }
        1
    }

    fn find_by_title(needle: &str) -> Option<Hwnd> {
        *NEEDLE.lock().ok()? = needle.to_string();
        *FOUND.lock().ok()? = None;
        unsafe {
            EnumWindows(enum_cb, 0);
        }
        *FOUND.lock().ok()?
    }

    // 把一块 HDC 区域读成 RGBA。
    unsafe fn read_dc(dc: Hdc, x: i32, y: i32, w: i32, h: i32) -> Result<Vec<u8>, String> {
        if w <= 0 || h <= 0 {
            return Err(format!("截图尺寸非法: {w}x{h}"));
        }
        let mem = CreateCompatibleDC(dc);
        if mem == 0 {
            return Err("CreateCompatibleDC 失败".to_string());
        }
        let bmp = CreateCompatibleBitmap(dc, w, h);
        if bmp == 0 {
            DeleteDC(mem);
            return Err("CreateCompatibleBitmap 失败".to_string());
        }
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(mem, 0, 0, w, h, dc, x, y, SRCCOPY | CAPTUREBLT);
        if ok == 0 {
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            return Err("BitBlt 失败（该窗口可能受保护或已最小化）".to_string());
        }
        let mut bi = BitmapInfo {
            header: BitmapInfoHeader {
                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                width: w,
                // 负高度 = top-down，省一次行翻转
                height: -h,
                planes: 1,
                bit_count: 32,
                compression: 0,
                size_image: 0,
                x_pels: 0,
                y_pels: 0,
                clr_used: 0,
                clr_important: 0,
            },
            colors: [0],
        };
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            buf.as_mut_ptr(),
            &mut bi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        if lines == 0 {
            return Err("GetDIBits 失败".to_string());
        }
        // BGRA -> RGBA
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(buf)
    }

    pub fn grab(title: Option<&str>, hwnd: Option<isize>) -> Result<(i32, i32, Vec<u8>, String), String> {
        unsafe {
            // 不做 DPI 感知的话，缩放 125%/150% 的机器上截出来是模糊的。
            SetProcessDPIAware();
        }

        // 1) 指定窗口
        let target = hwnd.or_else(|| title.and_then(find_by_title));
        if let Some(h) = target {
            if h != 0 {
                let mut r = Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                unsafe {
                    GetWindowRect(h, &mut r);
                }
                let w = r.right - r.left;
                let hh = r.bottom - r.top;
                if w <= 0 || hh <= 0 {
                    return Err("窗口尺寸为 0（最小化了？）".to_string());
                }
                // PrintWindow 对 GPU 渲染的窗口更靠谱；失败再退回窗口 DC。
                let pw = unsafe {
                    let dc = GetDC(0);
                    let mem = CreateCompatibleDC(dc);
                    let bmp = CreateCompatibleBitmap(dc, w, hh);
                    let old = SelectObject(mem, bmp);
                    let ok = PrintWindow(h, mem, PW_RENDERFULLCONTENT);
                    let mut out = Err("PrintWindow 失败".to_string());
                    if ok != 0 {
                        let mut bi = BitmapInfo {
                            header: BitmapInfoHeader {
                                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                                width: w,
                                height: -hh,
                                planes: 1,
                                bit_count: 32,
                                compression: 0,
                                size_image: 0,
                                x_pels: 0,
                                y_pels: 0,
                                clr_used: 0,
                                clr_important: 0,
                            },
                            colors: [0],
                        };
                        let mut buf = vec![0u8; (w as usize) * (hh as usize) * 4];
                        let lines =
                            GetDIBits(mem, bmp, 0, hh as u32, buf.as_mut_ptr(), &mut bi, DIB_RGB_COLORS);
                        if lines > 0 {
                            for px in buf.chunks_exact_mut(4) {
                                px.swap(0, 2);
                            }
                            out = Ok(buf);
                        }
                    }
                    SelectObject(mem, old);
                    DeleteObject(bmp);
                    DeleteDC(mem);
                    ReleaseDC(0, dc);
                    out
                };
                if let Ok(buf) = pw {
                    return Ok((w, hh, buf, format!("窗口句柄 {h}")));
                }
                let dc = unsafe { GetWindowDC(h) };
                let r = unsafe { read_dc(dc, 0, 0, w, hh) };
                unsafe {
                    ReleaseDC(h, dc);
                }
                return r.map(|b| (w, hh, b, format!("窗口句柄 {h}")));
            }
        }

        // 2) 全屏
        let w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let dc = unsafe { GetDC(0) };
        let r = unsafe { read_dc(dc, 0, 0, w, h) };
        unsafe {
            ReleaseDC(0, dc);
        }
        r.map(|b| (w, h, b, "全屏".to_string()))
    }
}
