use crate::{
    backend::pedals::pedal::Pedal,
    shared::{config::GLOBAL_CONFIG, pedals::WahWahParams},
};

pub struct WahWah {
    params: WahWahParams,
    lfo_phase: f32,
    // State variables for the biquad filter
    x1: f32, // Previous input
    x2: f32, // Input before previous
    y1: f32, // Previous output
    y2: f32, // Output before previous
}

impl WahWah {
    pub fn new(params: WahWahParams) -> Self {
        Self {
            params,
            lfo_phase: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn calculate_filter_coefficients(&self, frequency: f32) -> (f32, f32, f32, f32, f32) {
        let omega = 2.0 * std::f32::consts::PI * frequency / GLOBAL_CONFIG.sample_rate;
        let sin_omega = omega.sin();
        let cos_omega = omega.cos();
        let alpha = sin_omega / (2.0 * self.params.resonance);

        // Biquad bandpass filter coefficients
        let b0 = sin_omega / 2.0;
        let b1 = 0.0;
        let b2 = -sin_omega / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_omega;
        let a2 = 1.0 - alpha;

        // Normalize coefficients
        (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)
    }

    fn apply_biquad_filter(&mut self, input: f32, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) -> f32 {
        let output = b0 * input + b1 * self.x1 + b2 * self.x2 - a1 * self.y1 - a2 * self.y2;

        // Update state variables
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;

        output
    }
}

impl Pedal for WahWah {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32;

        // Update LFO phase
        self.lfo_phase += 2.0 * std::f32::consts::PI * self.params.lfo_rate / GLOBAL_CONFIG.sample_rate;
        if self.lfo_phase >= 2.0 * std::f32::consts::PI {
            self.lfo_phase -= 2.0 * std::f32::consts::PI;
        }

        // Calculate LFO modulation
        let lfo_value = self.lfo_phase.sin();
        let frequency_modulation = self.params.lfo_depth * lfo_value;
        
        // Apply frequency modulation to center frequency
        let modulated_frequency = self.params.frequency * (1.0 + frequency_modulation);
        
        // Clamp frequency to reasonable range
        let frequency = modulated_frequency.clamp(100.0, 8000.0);

        // Calculate filter coefficients
        let (b0, b1, b2, a1, a2) = self.calculate_filter_coefficients(frequency);

        // Apply the bandpass filter
        let filtered = self.apply_biquad_filter(input_f, b0, b1, b2, a1, a2);

        // Mix wet and dry signals
        let wet = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;
        let output = input_f * dry + filtered * wet;

        output.clamp(-32768.0, 32767.0) as i16
    }
}
