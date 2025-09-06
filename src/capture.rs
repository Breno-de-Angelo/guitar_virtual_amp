use alsa::{
    Direction, Error, PCM, ValueOr,
    pcm::{Access, Format, HwParams},
};
use crossbeam::channel::{Receiver, Sender};

use crate::pedal_chain::{self, Amp, Reverb};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum IOSelect {
    INPUT,
    OUTPUT,
}

pub struct AudioConfig {
    pub device_name: String,
    pub io_select: IOSelect,
    pub channels: u32,
    pub sample_rate: u32,
    pub period_size: i64,
    pub buffer_size: i64,
}

pub enum ControlMsg {
    AddAmp(f32),
    AddReverb(f32),
}

pub fn init_device(device_config: AudioConfig) -> Result<PCM, Box<dyn std::error::Error>> {
    let dir = if device_config.io_select == IOSelect::INPUT {
        Direction::Capture
    } else {
        Direction::Playback
    };
    let pcm = PCM::new(device_config.device_name.as_str(), dir, false).map_err(|e| {
        format!(
            "Falha ao abrir PCM '{}': {}",
            device_config.device_name.as_str(),
            e
        )
    })?;

    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(Format::s16())?;
        hwp.set_channels(device_config.channels)?;
        hwp.set_rate(device_config.sample_rate, ValueOr::Nearest)?;
        hwp.set_period_size(device_config.period_size, ValueOr::Nearest)?;
        hwp.set_buffer_size(device_config.buffer_size)?;
        pcm.hw_params(&hwp)?;
    }

    {
        let swp = pcm.sw_params_current()?;
        if device_config.io_select == IOSelect::OUTPUT {
            swp.set_start_threshold(1)?;
        }
        swp.set_avail_min(1)?;
        pcm.sw_params(&swp)?;
    }

    Ok(pcm)
}

pub fn playback(
    capture_pcm: PCM,
    playback_pcm: PCM,
    audio_tx: Sender<i16>,
    control_rx: Receiver<ControlMsg>,
) -> Result<(), Error> {
    let cap_io = capture_pcm.io_i16()?;
    let play_io = playback_pcm.io_i16()?;

    let in_period_frames = capture_pcm.hw_params_current()?.get_period_size()?;
    let out_period_frames = playback_pcm.hw_params_current()?.get_period_size()?;
    let mut in_buf = vec![0i16; in_period_frames as usize];
    let mut out_buf = vec![0i16; (out_period_frames as usize) * 2];

    let mut pedal_chain = pedal_chain::PedalChain::new();

    loop {
        if let Err(err) = cap_io.readi(&mut in_buf) {
            eprintln!("Erro na captura: {}", err);
            handle_xrun(err, &capture_pcm)?;
            continue;
        }

        for (i, &sample) in in_buf.iter().enumerate() {
            let processed_sample = pedal_chain.process_sample(sample);
            out_buf[i * 2] = processed_sample;
            out_buf[i * 2 + 1] = processed_sample;
            let _ = audio_tx.try_send(processed_sample);
        }

        match play_io.writei(&out_buf) {
            Ok(_) => (),
            Err(err) => {
                eprintln!("Erro no playback: {}", err);
                handle_xrun(err, &playback_pcm)?;
            }
        }

        for msg in control_rx.try_iter() {
            match msg {
                ControlMsg::AddReverb(val) => pedal_chain.add_pedal(Box::new(Reverb::new(val))),
                ControlMsg::AddAmp(val) => pedal_chain.add_pedal(Box::new(Amp::new(val))),
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
