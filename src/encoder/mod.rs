use std::{
    collections::BTreeMap,
    io::{Seek, SeekFrom, Write},
};

use crate::{
    error::TiffResult,
    tags::{CompressionMethod, PhotometricInterpretation, ResolutionUnit, SampleFormat, Tag},
    TiffError, TiffFormatError, TiffUnsupportedError,
};

mod compression;
pub mod value;

use compression::*;
use value::*;

struct DirectoryEntry {
    data_type: u16,
    count: u64,
    data: Vec<u8>,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    PackBits,
    Lzw,
    Deflate(DeflateLevel),
}
pub use compression::DeflateLevel;

/// Encoder for Tiff and BigTiff files.
///
/// With this type you can get a `ImageEncoder` to encode Tiff/BigTiff ifd directories with images.
///
/// # Examples
/// ```
/// # fn main() {
/// # let mut file = std::io::Cursor::new(Vec::new());
/// # let image_data = vec![0; 100*100*3];
/// use tiff::encoder::*;
///
/// // create a standard Tiff file
/// let mut tiff = TiffEncoder::new(&mut file).unwrap();
/// tiff.write_image(100, 100, 8, PhotometricInterpretation::RGB, &image_data).unwrap();
///
/// // create a BigTiff file
/// let mut bigtiff = TiffEncoder::new_big(&mut file).unwrap();
/// tiff.write_image(100000, 100000, 8, PhotometricInterpretation::RGB, &image_data).unwrap();
///
/// # }
/// ```
pub struct TiffEncoder<W: Write + Seek> {
    writer: W,
    big_tiff: bool,
    ifd_pointer_pos: Option<u64>,
    // We use BTreeMap to make sure tags are written in correct order
    ifd: BTreeMap<u16, DirectoryEntry>,
    compression: Compression,
    rows_per_strip: Option<u32>,
    sample_format: SampleFormat,
    bits_per_sample: u8,
    extra_samples: u16,
}

/// Constructor functions to create standard Tiff files.
impl<W: Write + Seek> TiffEncoder<W> {
    /// Creates a new encoder for standard Tiff files.
    ///
    /// To create BigTiff files, use [`new_big`][TiffEncoder::new_big].
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            big_tiff: false,
            ifd_pointer_pos: None,
            ifd: BTreeMap::new(),
            compression: Compression::None,
            rows_per_strip: None,
            sample_format: SampleFormat::Uint,
            bits_per_sample: 8,
            extra_samples: 0,
        }
    }

    /// Creates a new encoder for BigTiff files.
    ///
    /// To create standard Tiff files, use [`new`][TiffEncoder::new].
    pub fn new_big(writer: W) -> Self {
        Self {
            writer,
            big_tiff: true,
            ifd_pointer_pos: None,
            ifd: BTreeMap::new(),
            compression: Compression::None,
            rows_per_strip: None,
            sample_format: SampleFormat::Uint,
            bits_per_sample: 8,
            extra_samples: 0,
        }
    }

    pub fn set_compression(&mut self, compression: Compression) {
        self.compression = compression;
    }

    pub fn set_sample_format(&mut self, sample_format: SampleFormat) {
        self.sample_format = sample_format;
    }

    pub fn set_bits_per_sample(&mut self, bits_per_sample: u8) {
        self.bits_per_sample = bits_per_sample;
    }

    pub fn set_extra_samples(&mut self, extra_samples: u16) {
        self.extra_samples = extra_samples;
    }

    /// Set image number of lines per strip
    pub fn set_rows_per_strip(&mut self, value: u32) {
        self.rows_per_strip = Some(value);
    }

    /// Set image resolution
    pub fn set_resolution(&mut self, unit: ResolutionUnit, value: Rational) {
        self.set_tag(Tag::ResolutionUnit, unit.to_u16());
        self.set_tag(Tag::XResolution, value.clone());
        self.set_tag(Tag::YResolution, value);
    }

    /// Set image resolution unit
    pub fn resolution_unit(&mut self, unit: ResolutionUnit) {
        self.set_tag(Tag::ResolutionUnit, unit.to_u16())
    }

    /// Set image x-resolution
    pub fn x_resolution(&mut self, value: Rational) {
        self.set_tag(Tag::XResolution, value)
    }

    /// Set image y-resolution
    pub fn y_resolution(&mut self, value: Rational) {
        self.set_tag(Tag::YResolution, value)
    }

    /// Write a single ifd tag.
    pub fn set_tag<T: TiffValue>(&mut self, tag: Tag, value: T) {
        self.ifd.insert(
            tag.to_u16(),
            DirectoryEntry {
                data_type: <T>::FIELD_TYPE.to_u16(),
                count: value.count() as u64,
                data: value.data().to_vec(),
            },
        );
    }

    pub fn write_image(
        &mut self,
        width: u32,
        height: u32,
        photometric_interpretation: PhotometricInterpretation,
        data: &[u8],
    ) -> TiffResult<()> {
        self.new_image(width, height, photometric_interpretation)?
            .write(data)
    }

    pub fn new_image(
        &mut self,
        width: u32,
        height: u32,
        photometric_interpretation: PhotometricInterpretation,
    ) -> TiffResult<SubfileEncoder<W>> {
        if width == 0 || height == 0 {
            return Err(TiffError::FormatError(TiffFormatError::InvalidDimensions(
                width, height,
            )));
        }
        if self.compression == Compression::PackBits
            && self.rows_per_strip.map(|r| r > 1).unwrap_or(false)
        {
            return Err(TiffError::UnsupportedError(
                TiffUnsupportedError::UnsupportedCompressionMethod(CompressionMethod::PackBits),
            ));
        }

        let samples_per_pixel: u16 = self.extra_samples
            + match photometric_interpretation {
                PhotometricInterpretation::BlackIsZero
                | PhotometricInterpretation::WhiteIsZero
                | PhotometricInterpretation::TransparencyMask => 1,
                PhotometricInterpretation::RGB
                | PhotometricInterpretation::YCbCr
                | PhotometricInterpretation::CIELab => 3,
                PhotometricInterpretation::CMYK => 4,
                _ => {
                    return Err(TiffError::UnsupportedError(
                        TiffUnsupportedError::UnsupportedInterpretation(photometric_interpretation),
                    ))
                }
            };

        let row_samples = width as u64 * samples_per_pixel as u64;
        let row_bytes = (row_samples * self.bits_per_sample as u64).div_ceil(8);

        // Limit the strip size to prevent potential memory and security issues.
        // Also keep the multiple strip handling 'oiled'
        let rows_per_strip = self.rows_per_strip.unwrap_or({
            match self.compression {
                Compression::PackBits => 1, // Each row must be packed separately. Do not compress across row boundaries
                _ => 1_000_000u64.div_ceil(row_bytes) as u32,
            }
        });

        self.set_tag(Tag::ImageWidth, width);
        self.set_tag(Tag::ImageLength, height);

        let compression = match self.compression {
            Compression::None => CompressionMethod::None,
            Compression::PackBits => CompressionMethod::PackBits,
            Compression::Lzw => CompressionMethod::LZW,
            Compression::Deflate(_) => CompressionMethod::Deflate,
        };
        self.set_tag(Tag::Compression, compression.to_u16());

        self.set_tag(
            Tag::PhotometricInterpretation,
            photometric_interpretation.to_u16(),
        );
        self.set_tag(Tag::SamplesPerPixel, samples_per_pixel);
        self.set_tag(Tag::SampleFormat, self.sample_format.to_u16());
        self.set_tag(
            Tag::BitsPerSample,
            &*(0..samples_per_pixel)
                .map(|_| self.bits_per_sample as u16)
                .collect::<Vec<u16>>(),
        );

        self.set_tag(Tag::RowsPerStrip, rows_per_strip);

        if !self.ifd.contains_key(&Tag::XResolution.to_u16()) {
            self.set_tag(Tag::XResolution, Rational { n: 1, d: 1 });
        }
        if !self.ifd.contains_key(&Tag::YResolution.to_u16()) {
            self.set_tag(Tag::YResolution, Rational { n: 1, d: 1 });
        }
        if !self.ifd.contains_key(&Tag::ResolutionUnit.to_u16()) {
            self.set_tag(Tag::ResolutionUnit, ResolutionUnit::None.to_u16());
        }

        self.ifd.remove(&Tag::StripByteCounts.to_u16());
        self.ifd.remove(&Tag::StripOffsets.to_u16());
        self.ifd.remove(&Tag::TileByteCounts.to_u16());
        self.ifd.remove(&Tag::TileOffsets.to_u16());

        Ok(SubfileEncoder {
            inner: self,
            bytes_per_chunk: row_bytes as usize * rows_per_strip as usize,
            num_chunks: height.div_ceil(rows_per_strip as u32),
        })
    }
}

/// Type to encode images strip by strip.
pub struct SubfileEncoder<'a, W: Write + Seek> {
    inner: &'a mut TiffEncoder<W>,
    bytes_per_chunk: usize,
    num_chunks: u32,
}

impl<'a, W: Write + Seek> SubfileEncoder<'a, W> {
    pub fn write(self, data: &[u8]) -> TiffResult<()> {
        if self.inner.compression == Compression::None {
            let encoded: Vec<_> = data.chunks(self.bytes_per_chunk).collect();
            return self.write_encoded(&encoded);
        }

        let mut encoded = Vec::new();
        for chunk in data.chunks(self.bytes_per_chunk) {
            encoded.push(self.compress(chunk)?);
        }

        let encoded: Vec<_> = encoded.iter().map(|v| &**v).collect();
        self.write_encoded(&encoded)
    }

    fn compress(&self, data: &[u8]) -> TiffResult<Vec<u8>> {
        let mut compressed = Vec::new();
        match self.inner.compression {
            Compression::None => return Ok(data.to_vec()),
            Compression::PackBits => {
                Packbits.write_to(&mut compressed, data)?;
            }
            Compression::Lzw => {
                Lzw.write_to(&mut compressed, data)?;
            }
            Compression::Deflate(level) => {
                Deflate::with_level(level).write_to(&mut compressed, data)?;
            }
        }
        Ok(compressed)
    }

    pub fn encode_chunk(&self, chunk: &[u8]) -> TiffResult<Vec<u8>> {
        self.compress(chunk)
    }

    pub fn write_encoded(self, chunks: &[&[u8]]) -> TiffResult<()> {
        assert_eq!(self.num_chunks, chunks.len() as u32);

        let writer = &mut self.inner.writer;
        let ifd = &mut self.inner.ifd;
        let big_tiff = self.inner.big_tiff;
        let data_bytes = if big_tiff { 8 } else { 4 };

        let write_offset = |writer: &mut W, offset: u64| -> TiffResult<()> {
            if big_tiff {
                writer.write_all(&offset.to_le_bytes())?;
            } else {
                writer.write_all(&(offset as u32).to_le_bytes())?;
            }
            Ok(())
        };

        // Write header if not already written.
        if self.inner.ifd_pointer_pos.is_none() {
            if big_tiff {
                writer.write_all(&[0x49, 0x49, 43, 0, 8, 0, 0, 0])?;
            } else {
                writer.write_all(&[0x49, 0x49, 42, 0])?;
            }
            let offset = writer.stream_position()?;
            write_offset(writer, data_bytes + offset)?;
        }

        // Compute the sizes of different elements.
        let ifd_size = if big_tiff {
            16 + 20 * (2 + ifd.len() as u64)
        } else {
            6 + 12 * (2 + ifd.len() as u64)
        };
        let ifd_data_start = writer.stream_position()? + ifd_size;

        let chunk_ranges_size = if chunks.len() > 1 {
            chunks.len() as u64 * data_bytes
        } else {
            0
        };
        let ifd_data_size = chunk_ranges_size
            + ifd
                .values()
                .map(|e| e.data.len() as u64)
                .filter(|&len| len > data_bytes)
                .map(|len| len.next_multiple_of(data_bytes))
                .sum::<u64>();
        let pixel_data_start = ifd_data_start + ifd_data_size;

        // Compute chunk ranges.
        let mut offset = pixel_data_start;
        let mut chunk_sizes = Vec::new();
        let mut chunk_offsets = Vec::new();
        for chunk in chunks {
            if big_tiff {
                chunk_sizes.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
                chunk_offsets.extend_from_slice(&offset.to_le_bytes());
            } else {
                chunk_sizes.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                chunk_offsets.extend_from_slice(&(offset as u32).to_le_bytes());
            }
            offset += (chunk.len() as u64).next_multiple_of(data_bytes);
        }
        ifd.insert(
            Tag::StripByteCounts.to_u16(),
            DirectoryEntry {
                data_type: if big_tiff { 16 } else { 4 },
                count: chunks.len() as u64,
                data: chunk_sizes,
            },
        );
        ifd.insert(
            Tag::StripOffsets.to_u16(),
            DirectoryEntry {
                data_type: if big_tiff { 16 } else { 4 },
                count: chunks.len() as u64,
                data: chunk_offsets,
            },
        );

        // Initialize the IFD data.
        let mut entry_offsets = Vec::new();
        let mut entry_data = Vec::new();
        for entry in ifd.values() {
            if entry.data.len() > data_bytes as usize {
                entry_offsets.push(ifd_data_start + entry_data.len() as u64);
                entry_data.extend_from_slice(&entry.data);
                entry_data.resize(entry_data.len().next_multiple_of(data_bytes as usize), 0);
            } else {
                let mut data = [0; 8];
                data[..entry.data.len()].copy_from_slice(&entry.data);
                entry_offsets.push(u64::from_le_bytes(data));
            }
        }

        // Write the IFD.
        if big_tiff {
            writer.write_all(&(ifd.len() as u64).to_le_bytes())?;
        } else {
            writer.write_all(&(ifd.len() as u16).to_le_bytes())?;
        }
        for ((tag, entry), offset) in ifd.iter().zip(entry_offsets) {
            writer.write_all(&tag.to_le_bytes())?;
            writer.write_all(&entry.data_type.to_le_bytes())?;
            write_offset(writer, entry.count)?;
            write_offset(writer, offset)?;
        }
        let new_pointer = writer.stream_position()?;
        write_offset(writer, 0)?;

        // Write the IFD data.
        writer.write_all(&entry_data)?;

        // Write pixel data.
        for chunk in chunks {
            writer.write_all(chunk)?;
        }

        // Update the pointer to the IFD.
        if let Some(pos) = self.inner.ifd_pointer_pos {
            let old_position = writer.stream_position()?;
            writer.seek(SeekFrom::Start(pos))?;
            write_offset(writer, old_position)?;
            writer.seek(SeekFrom::Start(old_position))?;
        }
        self.inner.ifd_pointer_pos = Some(new_pointer);

        Ok(())
    }
}
