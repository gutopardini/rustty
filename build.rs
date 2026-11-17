fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let object = out.join("ft_glyph.o");
    let archive = out.join("libft_glyph.a");
    let cc = std::process::Command::new("cc")
        .args(["-O2", "-fPIC", "-I/usr/include/freetype2", "-c", "src/ft_glyph.c", "-o"])
        .arg(&object)
        .status()
        .expect("cc");
    assert!(cc.success(), "compilando o raster FreeType");
    let ar = std::process::Command::new("ar")
        .arg("rcs")
        .arg(&archive)
        .arg(&object)
        .status()
        .expect("ar");
    assert!(ar.success(), "arquivando o raster FreeType");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=ft_glyph");
    println!("cargo:rustc-link-lib=dylib=freetype");
    println!("cargo:rerun-if-changed=src/ft_glyph.c");
}
