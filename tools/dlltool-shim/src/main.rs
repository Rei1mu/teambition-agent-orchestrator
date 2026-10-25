// dlltool shim。
// 本机 rustup gnu 工具链自带的 dlltool 坏了（CreateProcess 失败）。
// MSVC 的 lib.exe 可以把 .def 转成 import lib。这个 shim 冒名 dlltool，
// 解析 -d/-D/-l 参数，转调 lib.exe /DEF。
use std::env;
use std::process::{exit, Command};

fn find_lib_exe() -> String {
    if let Ok(p) = env::var("TBHUB_LIB_EXE") {
        if !p.is_empty() {
            return p;
        }
    }
    // 常见 MSVC 路径
    let guesses = [
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\lib.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.40.33807\bin\HostX64\x64\lib.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Tools\MSVC\14.40.33807\bin\HostX64\x64\lib.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Tools\MSVC\14.40.33807\bin\HostX64\x64\lib.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.40.33807\bin\HostX64\x64\lib.exe",
    ];
    for g in guesses {
        if std::path::Path::new(g).exists() {
            return g.to_string();
        }
    }
    "lib.exe".to_string()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut def: Option<String> = None;
    let mut lib: Option<String> = None;
    let mut dll: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-d" => {
                i += 1;
                if i < args.len() {
                    def = Some(args[i].clone());
                }
            }
            "-l" => {
                i += 1;
                if i < args.len() {
                    lib = Some(args[i].clone());
                }
            }
            "-D" => {
                i += 1;
                if i < args.len() {
                    dll = Some(args[i].clone());
                }
            }
            _ => {}
        }
        i += 1;
    }

    let def = match def {
        Some(d) => d,
        None => {
            eprintln!("dlltool-shim: 缺少 -d <def>");
            exit(1);
        }
    };
    let lib = match lib {
        Some(l) => l,
        None => {
            eprintln!("dlltool-shim: 缺少 -l <lib>");
            exit(1);
        }
    };

    let text = match std::fs::read_to_string(&def) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("dlltool-shim: 读 def 失败 {def}: {e}");
            exit(1);
        }
    };

    // lib.exe 要求 def 里有 LIBRARY 段。没有就用 -D 补上。
    let has_library = text
        .lines()
        .any(|l| l.trim_start().to_uppercase().starts_with("LIBRARY"));
    let effective_def = if has_library {
        def.clone()
    } else {
        let dll = dll.unwrap_or_else(|| "unknown.dll".to_string());
        let patched = format!("LIBRARY {}\n{}", dll, text);
        let tmp = format!("{}.patched.def", def);
        if std::fs::write(&tmp, patched).is_err() {
            def.clone()
        } else {
            tmp
        }
    };

    let lib_exe = find_lib_exe();
    let status = Command::new(&lib_exe)
        .arg("/NOLOGO")
        .arg(format!("/DEF:{}", effective_def))
        .arg(format!("/OUT:{}", lib))
        .arg("/MACHINE:X64")
        .status();

    match status {
        Ok(s) if s.success() => exit(0),
        Ok(s) => {
            eprintln!("dlltool-shim: {lib_exe} 退出码 {:?}", s.code());
            exit(1);
        }
        Err(e) => {
            eprintln!("dlltool-shim: 无法运行 {lib_exe}: {e}");
            exit(1);
        }
    }
}
