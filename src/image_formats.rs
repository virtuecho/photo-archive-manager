use std::sync::Once;

static REGISTER_HEIF_DECODER: Once = Once::new();

pub fn register_heif_decoder() {
    REGISTER_HEIF_DECODER.call_once(|| {
        heifer::image_crate::register_image_decoder_hooks_with_options(heifer::Options {
            max_threads: 2,
            max_pixels: 50_000_000,
        });
    });
}
