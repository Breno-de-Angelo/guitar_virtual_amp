use num_complex::Complex;
use rustfft::FftPlanner;
const FFT_SIZE: usize = 2048;

fn run_fft_processor(rx: mpsc::Receiver<Vec<i16>>, shared_data: Arc<Mutex<SharedAudioData>>) {
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    // Buffer para os dados complexos que a FFT precisa
    let mut complex_buf = vec![Complex::new(0.0, 0.0); FFT_SIZE];

    while let Ok(waveform_i16) = rx.recv() {
        // 1. Converte e aplica uma função de janela (Hann) para melhores resultados
        for (i, sample) in waveform_i16.iter().enumerate().take(FFT_SIZE) {
            let sample_f32 = *sample as f32 / i16::MAX as f32; // Normaliza para [-1.0, 1.0]

            // Função de Janela de Hann para reduzir "vazamento espectral"
            let window_factor =
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
            complex_buf[i] = Complex::new(sample_f32 * window_factor, 0.0);
        }

        // 2. Processa a FFT
        fft.process(&mut complex_buf);

        // 3. Calcula as magnitudes. Só precisamos da primeira metade dos resultados.
        let magnitudes: Vec<f32> = complex_buf
            .iter()
            .take(FFT_SIZE / 2)
            .map(|c| c.norm()) // .norm() calcula a magnitude |a + bi| = sqrt(a^2 + b^2)
            .collect();

        // 4. Atualiza o estado compartilhado
        if let Ok(mut data) = shared_data.lock() {
            data.waveform = waveform_i16;
            data.fft_magnitudes = magnitudes;
        }
    }
}
