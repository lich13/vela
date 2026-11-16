//! Incremental decoding shared by protocol observation and managed SSE delivery.
use std::io::{self, Write};
pub(super) enum Decoder<W: Write> {
    Plain(W),
    Brotli(Box<brotli::DecompressorWriter<W>>),
    Gzip(flate2::write::GzDecoder<W>),
    Deflate(flate2::write::ZlibDecoder<W>),
    Zstd(zstd::stream::write::Decoder<'static, W>),
}
impl<W: Write> Decoder<W> {
    pub fn new(sink: W, encoding: &str) -> (Self, bool) {
        (
            match encoding.trim().to_ascii_lowercase().as_str() {
                "" | "identity" => Self::Plain(sink),
                "br" => Self::Brotli(Box::new(brotli::DecompressorWriter::new(sink, 4096))),
                "gzip" => Self::Gzip(flate2::write::GzDecoder::new(sink)),
                "deflate" => Self::Deflate(flate2::write::ZlibDecoder::new(sink)),
                "zstd" => {
                    Self::Zstd(zstd::stream::write::Decoder::new(sink).expect("zstd decoder"))
                }
                _ => return (Self::Plain(sink), true),
            },
            false,
        )
    }
    pub fn sink(&mut self) -> &mut W {
        match self {
            Self::Plain(s) => s,
            Self::Brotli(d) => d.get_mut(),
            Self::Gzip(d) => d.get_mut(),
            Self::Deflate(d) => d.get_mut(),
            Self::Zstd(d) => d.get_mut(),
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.write_all(bytes),
            Self::Brotli(d) => d.write_all(bytes),
            Self::Gzip(d) => d.write_all(bytes),
            Self::Deflate(d) => d.write_all(bytes),
            Self::Zstd(d) => d.write_all(bytes).and_then(|()| d.flush()),
        }
    }
    pub fn finish(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(_) => Ok(()),
            Self::Brotli(d) => d.close(),
            Self::Gzip(d) => d.try_finish(),
            Self::Deflate(d) => d.try_finish(),
            Self::Zstd(d) => d.do_finish(),
        }
    }
}
