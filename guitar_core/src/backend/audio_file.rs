//! Decodes backing-track audio files (mp3, ogg/vorbis, flac, wav, m4a/aac)
//! into memory with Symphonia, downmixed to mono `f32`.

use std::io::Cursor;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// File extensions offered in the backing-track file picker.
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "ogg", "flac", "wav", "m4a", "aac"];

/// A fully decoded mono audio clip.
#[derive(Clone, Debug, Default)]
pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl DecodedAudio {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate.max(1) as f64
    }

    /// Linearly interpolated sample at `secs` (0.0 outside the clip).
    pub fn sample_at(&self, secs: f64) -> f32 {
        if secs < 0.0 {
            return 0.0;
        }
        let pos = secs * self.sample_rate as f64;
        let idx = pos as usize;
        let Some(&a) = self.samples.get(idx) else {
            return 0.0;
        };
        let b = self.samples.get(idx + 1).copied().unwrap_or(0.0);
        let frac = (pos - idx as f64) as f32;
        a + (b - a) * frac
    }
}

pub fn decode_file(path: &Path) -> Result<DecodedAudio, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Não foi possível abrir o áudio: {e}"))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    decode_source(Box::new(file), hint)
}

/// Decodes in-memory audio (e.g. a Guitar Pro 8 embedded backing track).
pub fn decode_bytes(data: Vec<u8>) -> Result<DecodedAudio, String> {
    decode_source(Box::new(Cursor::new(data)), Hint::new())
}

fn decode_source(source: Box<dyn MediaSource>, hint: Hint) -> Result<DecodedAudio, String> {
    let stream = MediaSourceStream::new(source, Default::default());
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Formato de áudio não suportado: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("O arquivo não tem nenhuma faixa de áudio")?;
    let track_id = track.id;
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Codec não suportado: {e}"))?;

    let mut samples = Vec::new();
    // Reused across packets; rebuilt when a packet needs more frames or
    // a different channel layout than the current buffer was made for.
    let mut buffer: Option<(SampleBuffer<f32>, usize, usize)> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("Erro ao ler o áudio: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Corrupt frames are skipped rather than aborting the whole file.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Erro ao decodificar o áudio: {e}")),
        };
        let spec = *decoded.spec();
        sample_rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let frames = decoded.capacity();
        let (buf, _, _) = match &mut buffer {
            Some(b) if b.1 >= frames && b.2 == channels => b,
            _ => buffer.insert((SampleBuffer::new(frames as u64, spec), frames, channels)),
        };
        buf.copy_interleaved_ref(decoded);
        samples.extend(
            buf.samples()
                .chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
    }
    if samples.is_empty() || sample_rate == 0 {
        return Err("Nenhum áudio decodificado".to_string());
    }
    Ok(DecodedAudio { samples, sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(rate: u32, channels: u16, frames: &[[i16; 2]]) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for frame in frames {
                for s in &frame[..channels as usize] {
                    writer.write_sample(*s).unwrap();
                }
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn decodes_stereo_wav_to_mono() {
        let frames: Vec<[i16; 2]> = (0..1000).map(|_| [16384, 0]).collect();
        let audio = decode_bytes(wav_bytes(22_050, 2, &frames)).unwrap();
        assert_eq!(audio.sample_rate, 22_050);
        assert_eq!(audio.samples.len(), 1000);
        // Left at half scale, right silent: mono average is a quarter scale.
        assert!((audio.samples[10] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn sample_at_interpolates_and_is_silent_outside() {
        let audio = DecodedAudio { samples: vec![0.0, 1.0], sample_rate: 2 };
        assert_eq!(audio.sample_at(-1.0), 0.0);
        assert!((audio.sample_at(0.25) - 0.5).abs() < 1e-6);
        assert_eq!(audio.sample_at(5.0), 0.0);
    }
}
