//! Raster FreeType para glifos de símbolo. O fontdue preenche contornos
//! sobrepostos da Nerd Font com furos; o FreeType desenha o ícone inteiro.

use std::ffi::c_void;

pub struct GlyphImage {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub left: i32,
    pub top: i32,
}

pub struct Face {
    _bytes: Vec<u8>,
    library: *mut c_void,
    face: *mut c_void,
}

unsafe impl Send for Face {}

impl Face {
    pub fn open(bytes: Vec<u8>) -> Option<Self> {
        unsafe {
            let library = ft_library_new();
            if library.is_null() {
                return None;
            }
            let face = ft_face_new(library, bytes.as_ptr(), bytes.len() as libc::c_long);
            if face.is_null() {
                ft_library_free(library);
                return None;
            }
            Some(Self {
                _bytes: bytes,
                library,
                face,
            })
        }
    }

    pub fn has(&self, ch: char) -> bool {
        unsafe { ft_face_has(self.face, ch as libc::c_ulong) != 0 }
    }

    pub fn render(&mut self, ch: char, px: f32) -> Option<GlyphImage> {
        let px = px.round().clamp(1.0, 128.0) as libc::c_uint;
        let mut width = 0;
        let mut height = 0;
        let mut left = 0;
        let mut top = 0;
        unsafe {
            let buffer = ft_render(self.face, ch as libc::c_ulong, px, &mut width, &mut height, &mut left, &mut top);
            if buffer.is_null() || width <= 0 || height <= 0 {
                return None;
            }
            let len = (width as usize).saturating_mul(height as usize);
            let pixels = std::slice::from_raw_parts(buffer, len).to_vec();
            ft_buffer_free(buffer);
            Some(GlyphImage {
                pixels,
                width: width as usize,
                height: height as usize,
                left,
                top,
            })
        }
    }
}

impl Drop for Face {
    fn drop(&mut self) {
        unsafe {
            ft_face_free(self.face);
            ft_library_free(self.library);
        }
    }
}

unsafe extern "C" {
    fn ft_library_new() -> *mut c_void;
    fn ft_library_free(library: *mut c_void);
    fn ft_face_new(library: *mut c_void, bytes: *const u8, len: libc::c_long) -> *mut c_void;
    fn ft_face_free(face: *mut c_void);
    fn ft_face_has(face: *mut c_void, codepoint: libc::c_ulong) -> libc::c_int;
    fn ft_render(
        face: *mut c_void,
        codepoint: libc::c_ulong,
        px: libc::c_uint,
        width: *mut libc::c_int,
        height: *mut libc::c_int,
        left: *mut libc::c_int,
        top: *mut libc::c_int,
    ) -> *mut u8;
    fn ft_buffer_free(buffer: *mut u8);
}
