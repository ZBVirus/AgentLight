//! Minimal, dependency-free WAV amplitude scaling.
//!
//! The desktop shell applies the user's alarm volume by scaling the samples of
//! the sound it is about to play, rather than touching the system or per-app
//! mixer. That keeps the setting scoped to AgentLight's own alarm. The routine
//! is pure and lives here so it is unit-tested on every host without the GUI
//! toolchain.

/// Scale a 16-bit PCM WAV's samples by `volume` (a percentage, clamped to
/// 0-100) and return a new buffer.
///
/// Only uncompressed PCM (`fmt ` audio format 1) with 16-bit samples is scaled;
/// anything else is returned as an unchanged copy so playback still works. A
/// volume of 100 also returns an unchanged copy; 0 silences the data chunk.
pub fn scale_wav_pcm16(bytes: &[u8], volume: u8) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let volume = volume.min(100);
    if volume >= 100 {
        return out;
    }
    // A canonical header is "RIFF" <size> "WAVE"; everything after is chunks.
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return out;
    }

    let mut audio_format = 0u16;
    let mut bits = 0u16;
    let mut data: Option<(usize, usize)> = None;
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        if id == b"fmt " && size >= 16 && body + 16 <= bytes.len() {
            audio_format = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
            bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            data = Some((body, (body + size).min(bytes.len())));
            break;
        }
        // Chunks are word-aligned: an odd size is padded with one byte.
        pos = body + size + (size & 1);
    }

    if audio_format != 1 || bits != 16 {
        return out;
    }
    let Some((start, end)) = data else {
        return out;
    };

    let gain = volume as f32 / 100.0;
    let mut i = start;
    while i + 1 < end {
        let sample = i16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let scaled = (sample as f32 * gain).round().clamp(-32768.0, 32767.0) as i16;
        let encoded = scaled.to_le_bytes();
        out[i] = encoded[0];
        out[i + 1] = encoded[1];
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny mono 16-bit PCM WAV from raw samples.
    fn wav(samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&22050u32.to_le_bytes());
        out.extend_from_slice(&44100u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn samples(bytes: &[u8]) -> Vec<i16> {
        // Data chunk starts after the fixed 44-byte header in these fixtures.
        bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn scales_samples_by_volume() {
        let source = wav(&[1000, -1000, 32767, -32768]);
        let half = scale_wav_pcm16(&source, 50);
        assert_eq!(samples(&half), vec![500, -500, 16384, -16384]);
        // The header is preserved byte for byte.
        assert_eq!(&half[..44], &source[..44]);
    }

    #[test]
    fn full_volume_is_unchanged() {
        let source = wav(&[1234, -4321]);
        assert_eq!(scale_wav_pcm16(&source, 100), source);
    }

    #[test]
    fn zero_volume_silences() {
        let source = wav(&[1000, -1000]);
        assert_eq!(samples(&scale_wav_pcm16(&source, 0)), vec![0, 0]);
    }

    #[test]
    fn clamps_to_full_volume() {
        let source = wav(&[1000]);
        assert_eq!(scale_wav_pcm16(&source, 250), source);
    }

    #[test]
    fn non_wav_passes_through() {
        let bytes = b"not a wav file".to_vec();
        assert_eq!(scale_wav_pcm16(&bytes, 50), bytes);
    }

    #[test]
    fn non_pcm_wav_passes_through() {
        let mut source = wav(&[1000, -1000]);
        // Mark the fmt chunk as a non-PCM codec (e.g. IEEE float).
        source[20] = 3;
        assert_eq!(scale_wav_pcm16(&source, 50), source);
    }
}
