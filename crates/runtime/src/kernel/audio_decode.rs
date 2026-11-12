//! `audio/decode`: encoded audio bytes (mp3, wav, flac, ogg, aac, mp4) to interleaved f32 PCM, answered by the kernel.
use orbital_core_base::os::{OsError, OsErrorKind};

pub(super) struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub samples: Vec<f32>,
}

#[cfg(not(feature = "std"))]
pub(super) fn decode(_bytes: &[u8], _mono: bool) -> Result<Pcm, OsError> {
    Err(OsError::new(
        OsErrorKind::Unsupported,
        "audio decode is not built into this kernel",
    ))
}

#[cfg(feature = "std")]
fn invalid(error: impl std::fmt::Display) -> OsError {
    OsError::new(OsErrorKind::Invalid, error.to_string())
}

#[cfg(feature = "std")]
pub(super) fn decode(bytes: &[u8], mono: bool) -> Result<Pcm, OsError> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        Default::default(),
    );
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            source,
            &FormatOptions { enable_gapless: true, ..FormatOptions::default() },
            &MetadataOptions::default(),
        )
        .map_err(invalid)?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| invalid("the data holds no audio track"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(invalid)?;
    let mut spec = None;
    let mut interleaved: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(error) => return Err(invalid(error)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(invalid)?;
        let signal = *decoded.spec();
        spec.get_or_insert(signal);
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, signal);
        buffer.copy_interleaved_ref(decoded);
        interleaved.extend_from_slice(buffer.samples());
    }
    let spec = spec.ok_or_else(|| invalid("the data decodes to no audio"))?;
    let channels = u16::try_from(spec.channels.count())
        .map_err(|_| invalid("the channel count exceeds the supported range"))?;
    if channels == 0 {
        return Err(invalid("the data has no channels"));
    }
    let width = usize::from(channels);
    if !mono || width == 1 {
        let frames = (interleaved.len() / width) as u64;
        return Ok(Pcm {
            sample_rate: spec.rate,
            channels,
            frames,
            samples: interleaved,
        });
    }
    let samples: Vec<f32> = interleaved
        .chunks_exact(width)
        .map(|frame| frame.iter().sum::<f32>() / width as f32)
        .collect();
    Ok(Pcm {
        sample_rate: spec.rate,
        channels: 1,
        frames: samples.len() as u64,
        samples,
    })
}
