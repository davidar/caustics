# JPEG XL fixtures

Tiny lossless pictures for the decoder's tests in `../tests.rs`, written by
ImageMagick 7 (`magick`, built with libjxl), so that the decoder is checked
against an encoder that is not its own. Lossless at `-quality 100`, so every
pixel is exact (all but the last). From this directory:

```sh
# 4x2, opaque: red, green, blue, white over black, grey, yellow, cyan.
magick -size 1x1 xc:'#ff0000' xc:'#00ff00' xc:'#0000ff' xc:'#ffffff' +append \
    \( xc:'#000000' xc:'#808080' xc:'#ffff00' xc:'#00ffff' +append \) -append \
    -depth 8 -quality 100 opaque.jxl
# The same at sixteen bits a sample.
magick opaque.jxl -depth 16 -quality 100 deep.jxl
# 4x2 with alpha 1, 0.5, 0, 0.25 over 1, 0.5, 1, 0.75.
magick -size 1x1 xc:'rgba(255,0,0,1)' xc:'rgba(0,255,0,0.5)' xc:'rgba(0,0,255,0)' \
    xc:'rgba(255,255,255,0.25)' +append \( xc:'rgba(0,0,0,1)' \
    xc:'rgba(128,128,128,0.5)' xc:'rgba(255,255,0,1)' xc:'rgba(0,255,255,0.75)' \
    +append \) -append -depth 8 -quality 100 alpha.jxl
# 4x1 grey: 0, 64, 128, 255.
magick -size 1x1 xc:'gray(0)' xc:'gray(64)' xc:'gray(128)' xc:'gray(255)' +append \
    -colorspace Gray -depth 8 -quality 100 grey.jxl
# 48x32 lossy (VarDCT), turned a quarter by its orientation: a red-to-blue
# ramp down, plus a green-to-black ramp across.
magick -size 48x32 gradient:'#ff0000-#0000ff' \( -size 48x32 \
    gradient:'#00ff00-#000000' -rotate 90 -resize 48x32! \) -compose plus \
    -composite -orient RightTop -quality 75 lossy.jxl
```

The last is lossy, so its test compares it with libjxl's own decode
(`magick lossy.jxl -auto-orient rgba:-`) within a few levels.

Each is a bare codestream (`FF 0A`); the tests wrap one in the container
themselves.
