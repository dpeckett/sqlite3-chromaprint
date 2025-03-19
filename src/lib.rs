//! SQLite3 extension for audio fingerprinting.
//!
//! This library provides two SQLite functions:
//!
//! 1. `fingerprint(path TEXT)`: Fingerprint an audio file at the given path.
//! 2. `compare_fingerprints(fingerprint_a TEXT, fingerprint_b TEXT)`: Compare two fingerprints.
//!
//! The fingerprints are generated using Chromaprint, a library for generating audio fingerprints.
//!     
//! # Example
//!
//! ```sql
//! .load target/debug/libsqlite3_chromaprint.so
//! SELECT compare_fingerprints(
//!   fingerprint('src/testdata/XC444467.ogg'),
//!   fingerprint('src/testdata/XC444467.mp3')
//! );
//! ```

use std::os::raw::{c_char, c_int};
use std::path::Path;

use anyhow::{Context, Result};
use base64::prelude::*;
use rusqlite::ffi;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{ToSqlOutput, Value, ValueRef};
use rusqlite::Connection;
use rusty_chromaprint::{match_fingerprints, Configuration, Fingerprinter};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[no_mangle]
pub unsafe extern "C" fn sqlite3_extension_init(
    db: *mut ffi::sqlite3,
    pz_err_msg: *mut *mut c_char,
    p_api: *mut ffi::sqlite3_api_routines,
) -> c_int {
    Connection::extension_init2(db, pz_err_msg, p_api, extension_init)
}

fn extension_init(db: Connection) -> rusqlite::Result<bool> {
    db.create_scalar_function(
        "fingerprint",
        1,
        FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let path = match ctx.get_raw(0) {
                ValueRef::Text(s) => Ok(std::path::Path::new(
                    std::str::from_utf8(s).map_err(rusqlite::Error::Utf8Error)?,
                )),
                v => Err(rusqlite::Error::InvalidFunctionParameterType(
                    0,
                    v.data_type(),
                )),
            }?;

            let fingerprint = fingerprint_file(Path::new(path))
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;

            Ok(ToSqlOutput::Owned(Value::Text(fingerprint)))
        },
    )?;

    db.create_scalar_function(
        "compare_fingerprints",
        2,
        FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let fingerprint_a: &str = match ctx.get_raw(0) {
                ValueRef::Text(s) => {
                    Ok(std::str::from_utf8(s).map_err(rusqlite::Error::Utf8Error)?)
                }
                v => Err(rusqlite::Error::InvalidFunctionParameterType(
                    0,
                    v.data_type(),
                )),
            }?;
            let fingerprint_b: &str = match ctx.get_raw(1) {
                ValueRef::Text(s) => {
                    Ok(std::str::from_utf8(s).map_err(rusqlite::Error::Utf8Error)?)
                }
                v => Err(rusqlite::Error::InvalidFunctionParameterType(
                    1,
                    v.data_type(),
                )),
            }?;

            let similarity_score = compare_fingerprints(fingerprint_a, fingerprint_b)
                .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;

            Ok(ToSqlOutput::Owned(Value::Real(
                similarity_score.unwrap_or(0.0),
            )))
        },
    )?;

    Ok(false)
}

fn fingerprint_file(path: &Path) -> Result<String> {
    let src = std::fs::File::open(path).context("Failed to open file")?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("Failed to probe format")?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("No audio track found")?;

    let sample_rate = track
        .codec_params
        .sample_rate
        .context("Missing sample rate")?;
    let channels = track
        .codec_params
        .channels
        .context("Missing channels")?
        .count();

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Failed to create decoder")?;

    let config = Configuration::preset_test1();
    let mut printer = Fingerprinter::new(&config);
    printer
        .start(sample_rate, channels as u32)
        .context("Failed to start fingerprinter")?;

    while let Ok(packet) = format.next_packet() {
        let decoded = decoder.decode(&packet).context("Failed to decode packet")?;
        let mut sample_buffer =
            SampleBuffer::<i16>::new(decoded.capacity() as u64, *decoded.spec());
        sample_buffer.copy_interleaved_ref(decoded);
        printer.consume(sample_buffer.samples());
    }

    printer.finish();
    let fingerprint = printer.fingerprint();
    let fingerprint: Vec<u8> = fingerprint
        .iter()
        .flat_map(|&x| x.to_be_bytes().to_vec())
        .collect();
    Ok(BASE64_STANDARD.encode(&fingerprint))
}

fn compare_fingerprints(fingerprint_a: &str, fingerprint_b: &str) -> Result<Option<f64>> {
    let fingerprint_a = BASE64_STANDARD
        .decode(fingerprint_a.trim())
        .context("Base64 decode error for fingerprint_a")?;
    let fingerprint_b = BASE64_STANDARD
        .decode(fingerprint_b.trim())
        .context("Base64 decode error for fingerprint_b")?;

    let fingerprint_a: Vec<u32> = fingerprint_a
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes(chunk.try_into().unwrap()))
        .collect();

    let fingerprint_b: Vec<u32> = fingerprint_b
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes(chunk.try_into().unwrap()))
        .collect();

    let config = Configuration::preset_test1();
    let segments = match_fingerprints(&fingerprint_a, &fingerprint_b, &config)
        .context("Failed to match fingerprints")?;

    if segments.is_empty() {
        return Ok(None);
    }

    let total_duration: f64 = segments.iter().map(|s| s.duration(&config) as f64).sum();
    let similarity_score = 32.0
        - (total_duration
            / segments
                .iter()
                .map(|s| s.duration(&config) as f64 / (32.0 - s.score))
                .sum::<f64>());

    Ok(Some(similarity_score))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_file() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

        let fingerprint_a =
            fingerprint_file(&Path::new(&manifest_dir).join("src/testdata/XC444467.ogg")).unwrap();

        let fingerprint_b =
            fingerprint_file(&Path::new(&manifest_dir).join("src/testdata/XC444467.mp3")).unwrap();

        let similarity_score = compare_fingerprints(&fingerprint_a, &fingerprint_b).unwrap();

        // Less is better, range approx. 0.0 - 32.0
        assert!(similarity_score.unwrap() < 2.0);
    }
}
