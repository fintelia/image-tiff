extern crate tiff;

use std::io::Cursor;
use tiff::{
    decoder::{Decoder, DecodingResult},
    encoder::{Compression, DeflateLevel, TiffEncoder},
    tags::PhotometricInterpretation,
};

fn encode_decode_with_compression(compression: Compression) {
    let mut image_rgb = vec![0u16; 3 * 7];
    for v in &mut image_rgb {
        *v = fastrand::u16(..);
    }

    let mut image_grayscale = vec![0u8; 21 * 10];
    for v in &mut image_grayscale {
        *v = fastrand::u8(..);
    }

    // Encode tiff with compression
    let mut data = Cursor::new(Vec::new());
    {
        // Create a multipage image with 2 images
        let mut encoder = TiffEncoder::new(&mut data);
        encoder.set_compression(compression);

        encoder.set_bits_per_sample(16);
        encoder
            .write_image(
                1,
                7,
                PhotometricInterpretation::RGB,
                bytemuck::cast_slice(&image_rgb),
            )
            .unwrap();

        encoder.set_bits_per_sample(8);
        encoder
            .write_image(
                21,
                10,
                PhotometricInterpretation::BlackIsZero,
                &image_grayscale,
            )
            .unwrap();
    }

    // Decode tiff
    data.set_position(0);
    {
        let mut decoder = Decoder::new(data).unwrap();

        // Check the RGB image
        assert_eq!(
            match decoder.read_image() {
                Ok(DecodingResult::U16(image_data)) => image_data,
                unexpected => panic!("Descoding RGB failed: {:?}", unexpected),
            },
            image_rgb
        );

        // Check the grayscale image
        decoder.next_image().unwrap();
        assert_eq!(
            match decoder.read_image() {
                Ok(DecodingResult::U8(image_data)) => image_data,
                unexpected => panic!("Decoding grayscale failed: {:?}", unexpected),
            },
            image_grayscale
        );
    }
}

#[test]
fn encode_decode_without_compression() {
    encode_decode_with_compression(Compression::None);
}

#[test]
fn encode_decode_with_lzw() {
    encode_decode_with_compression(Compression::Lzw);
}

#[test]
fn encode_decode_with_deflate() {
    encode_decode_with_compression(Compression::Deflate(DeflateLevel::Fast));
    encode_decode_with_compression(Compression::Deflate(DeflateLevel::Balanced));
    encode_decode_with_compression(Compression::Deflate(DeflateLevel::Best));
}

#[test]
fn encode_decode_with_packbits() {
    encode_decode_with_compression(Compression::PackBits);
}
