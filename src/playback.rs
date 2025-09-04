use std::sync::mpsc::Sender;

use alsa::Error;
use alsa::pcm::PCM;

pub fn playback(capture_pcm: PCM, playback_pcm: PCM, tx: Sender<Vec<i16>>) -> Result<(), Error> {
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

        tx.send(in_buf.clone()).ok();

        for (i, &sample) in in_buf.iter().enumerate() {
            out_buf[i * 2] = sample;
            out_buf[i * 2 + 1] = sample;
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
