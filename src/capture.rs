use alsa::{
    Direction, Error, PCM, ValueOr,
    pcm::{Access, Format, HwParams},
};

use crate::buffer::AudioBuffer;

pub struct CaptureConfig {
    pub device_name: String,
}

pub struct PlaybackConfig {
    pub device_name: String,
    pub channels: u32,
    pub sample_rate: u32,
    pub period_size: i64,
    pub buffer_size: i64,
}

pub fn init_capture(
    capture_config: CaptureConfig,
    playback_config: PlaybackConfig,
) -> Result<(PCM, PCM), Box<dyn std::error::Error>> {
    let capture_pcm = PCM::new(
        capture_config.device_name.as_str(),
        Direction::Capture,
        false,
    )
    .map_err(|e| {
        format!(
            "Falha ao abrir PCM '{}': {}",
            capture_config.device_name.as_str(),
            e
        )
    })?;
    let playback_pcm = PCM::new(&playback_config.device_name, Direction::Playback, false)
        .or_else(|_| PCM::new("default", Direction::Playback, false))
        .or_else(|_| PCM::new("pulse", Direction::Playback, false))
        .map_err(|e| format!("Falha ao abrir dispositivo de playback: {}", e))?;

    {
        let hwp = HwParams::any(&playback_pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(Format::s16())?;
        hwp.set_channels(playback_config.channels)?;
        hwp.set_rate(playback_config.sample_rate, ValueOr::Nearest)?;
        hwp.set_period_size(playback_config.period_size, ValueOr::Nearest)?;
        hwp.set_buffer_size(playback_config.buffer_size)?;
        // hwp.set_period_size(FFT_SIZE as i64, ValueOr::Nearest)?;
        // hwp.set_buffer_size((FFT_SIZE * 2) as i64)?;
        playback_pcm.hw_params(&hwp)?;
    }

    {
        let swp = playback_pcm.sw_params_current()?;
        swp.set_start_threshold(1)?;
        swp.set_avail_min(1)?;
        playback_pcm.sw_params(&swp)?;
    }

    Ok((capture_pcm, playback_pcm))
}

pub fn playback(
    capture_pcm: PCM,
    playback_pcm: PCM,
    mut audio_buffer: AudioBuffer<f64>,
) -> Result<(), Error> {
    let cap_io = capture_pcm.io_i16()?;
    let play_io = playback_pcm.io_i16()?;

    let period_frames = capture_pcm.hw_params_current()?.get_period_size()?;
    let mut in_buf = vec![0i16; period_frames as usize];
    let mut out_buf = vec![0i16; (period_frames as usize) * 2];

    capture_pcm.prepare()?;
    playback_pcm.prepare()?;

    loop {
        if let Err(err) = cap_io.readi(&mut in_buf) {
            eprintln!("Erro na captura: {}", err);
            handle_xrun(err, &capture_pcm)?;
            continue;
        }

        for (i, &sample) in in_buf.iter().enumerate() {
            out_buf[i * 2] = sample;
            out_buf[i * 2 + 1] = sample;
            audio_buffer.push(sample as f64 / i16::MAX as f64);
        }

        match play_io.writei(&out_buf) {
            Ok(_) => (),
            Err(err) => {
                eprintln!("Erro no playback: {}", err);
                handle_xrun(err, &playback_pcm)?;
            }
        }
    }
}

/// Tenta se recuperar de um erro de XRUN (underrun/overrun).
/// Se o erro for recuperável, prepara o dispositivo novamente.
/// Se não for, propaga o erro para encerrar o programa.
fn handle_xrun(err: Error, pcm: &PCM) -> Result<(), Error> {
    if let alsa::nix::Error::EPIPE = err.nix_error() {
        // EPIPE é o código de erro para XRUN
        pcm.prepare()?;
        Ok(())
    } else {
        Err(err)
    }
}
