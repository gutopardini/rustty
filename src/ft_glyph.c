#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_IMAGE_H
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct FtLibrary FtLibrary;
typedef struct FtFace FtFace;

FtLibrary *ft_library_new(void) {
    FT_Library library;
    if (FT_Init_FreeType(&library)) {
        return NULL;
    }
    return (FtLibrary *)library;
}

void ft_library_free(FtLibrary *library) {
    if (library) {
        FT_Done_FreeType((FT_Library)library);
    }
}

FtFace *ft_face_new(FtLibrary *library, const unsigned char *bytes, long len) {
    FT_Face face;
    if (!library || !bytes || len <= 0) {
        return NULL;
    }
    if (FT_New_Memory_Face((FT_Library)library, bytes, len, 0, &face)) {
        return NULL;
    }
    return (FtFace *)face;
}

void ft_face_free(FtFace *face) {
    if (face) {
        FT_Done_Face((FT_Face)face);
    }
}

int ft_face_has(FtFace *face, unsigned long codepoint) {
    if (!face) {
        return 0;
    }
    return FT_Get_Char_Index((FT_Face)face, codepoint) != 0;
}

unsigned char *ft_render(FtFace *face, unsigned long codepoint, unsigned px, int *width, int *height, int *left, int *top) {
    if (!face || !width || !height || !left || !top || px == 0) {
        return NULL;
    }
    FT_Face ft_face = (FT_Face)face;
    if (FT_Set_Pixel_Sizes(ft_face, 0, px)) {
        return NULL;
    }
    /* Sem o overlap, contornos sobrepostos da Nerd Font (ícones e quadrados
       pixelados) saem furados. O tamanho pedido preserva a grade de pixels. */
    if (FT_Load_Char(ft_face, codepoint, FT_LOAD_NO_HINTING | FT_LOAD_NO_BITMAP)) {
        return NULL;
    }
    if (ft_face->glyph->format == FT_GLYPH_FORMAT_OUTLINE) {
        ft_face->glyph->outline.flags |= FT_OUTLINE_OVERLAP;
    }
    if (FT_Render_Glyph(ft_face->glyph, FT_RENDER_MODE_NORMAL)) {
        return NULL;
    }
    FT_Bitmap *bitmap = &ft_face->glyph->bitmap;
    if (bitmap->pixel_mode != FT_PIXEL_MODE_GRAY || bitmap->buffer == NULL || bitmap->width == 0 || bitmap->rows == 0) {
        return NULL;
    }
    int w = (int)bitmap->width;
    int h = (int)bitmap->rows;
    unsigned char *out = malloc((size_t)w * (size_t)h);
    if (!out) {
        return NULL;
    }
    int pitch = bitmap->pitch;
    for (int y = 0; y < h; y++) {
        const unsigned char *row;
        if (pitch >= 0) {
            row = bitmap->buffer + (size_t)y * (size_t)pitch;
        } else {
            row = bitmap->buffer + (size_t)(h - 1 - y) * (size_t)(-pitch);
        }
        memcpy(out + (size_t)y * (size_t)w, row, (size_t)w);
    }
    *width = w;
    *height = h;
    *left = ft_face->glyph->bitmap_left;
    *top = ft_face->glyph->bitmap_top;
    return out;
}

void ft_buffer_free(unsigned char *buffer) {
    free(buffer);
}
