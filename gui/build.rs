use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/runtime");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let runtime_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("assets")
        .join("runtime");
    let exe = runtime_dir.join("goodbyedpi.exe");
    let dll = runtime_dir.join("WinDivert.dll");
    let sys = runtime_dir.join("WinDivert64.sys");

    let generated = out_dir.join("embedded_runtime.rs");
    let code = if exe.is_file() && dll.is_file() && sys.is_file() {
        format!(
            "pub static EMBEDDED_GOODBYEDPI: Option<&'static [u8]> = Some(include_bytes!(r#\"{}\"#));\n\
             pub static EMBEDDED_WINDIVERT_DLL: Option<&'static [u8]> = Some(include_bytes!(r#\"{}\"#));\n\
             pub static EMBEDDED_WINDIVERT_SYS: Option<&'static [u8]> = Some(include_bytes!(r#\"{}\"#));\n",
            exe.display(),
            dll.display(),
            sys.display()
        )
    } else {
        "pub static EMBEDDED_GOODBYEDPI: Option<&'static [u8]> = None;\n\
         pub static EMBEDDED_WINDIVERT_DLL: Option<&'static [u8]> = None;\n\
         pub static EMBEDDED_WINDIVERT_SYS: Option<&'static [u8]> = None;\n"
            .to_owned()
    };
    fs::write(generated, code).expect("write embedded runtime module");

    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_manifest_file("assets/app.manifest");
        resource.compile().expect("compile Windows application manifest");
    }
}

