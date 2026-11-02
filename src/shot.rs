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

use std::path::Path;

#[cfg(windows)]
pub fn capture(out: &str, title: Option<&str>, hwnd: Option<isize>) -> Result<String, String> {
    let (w, h, rgba, what) = win::grab(title, hwnd)?;
    let png = png_encode(w as u32, h as u32, &rgba);
    let p = Path::new(out);
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::write(p, &png).map_err(|e| format!("写文件失败 {}: {e}", p.display()))?;
    Ok(format!(
        "已截图: {} ({}x{}, {:.1} KB)\n{}",
        p.display(),
        w,
        h,
        png.len() as f64 / 1024.0,
        what
    ))
}

#[cfg(not(windows))]
pub fn capture(_out: &str, _title: Option<&str>, _hwnd: Option<isize>) -> Result<String, String> {
    Err("截图只在 Windows 上可用".to_string())
}

// ---- 最小 PNG 编码 ----
//
// 只为截图服务：不压缩（deflate stored 块），不滤波，8 位 RGBA。
// 加 image / png crate 要多编一堆代码，而截图对体积不敏感。

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(data.len() + 12);
    v.extend_from_slice(&(data.len() as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(data);
    let mut both = Vec::with_capacity(4 + data.len());
    both.extend_from_slice(kind);
    both.extend_from_slice(data);
    v.extend_from_slice(&crc32(&both).to_be_bytes());
    v
}

fn png_encode(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    // 每行前面加一个 filter 字节（0 = None）。
    let stride = (w as usize) * 4;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }

    // zlib 头 + deflate stored 块 + adler32
    let mut z = vec![0x78u8, 0x01];
    let mut pos = 0usize;
    while pos < raw.len() {
        let n = (raw.len() - pos).min(65535);
        let last = pos + n == raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 位 / RGBA / deflate / 无滤波 / 无隔行

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &z));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
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
