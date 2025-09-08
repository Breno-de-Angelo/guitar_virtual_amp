use crate::{
    backend::{
        dsp::{all_pass::Allpass, comb_filter::CombFilter},
        pedals::pedal::Pedal,
    },
    shared::{config::GLOBAL_CONFIG, pedals::ReverbParams},
};

pub struct Reverb {
    combs: Vec<CombFilter>,
    allpasses: Vec<Allpass>,
    params: ReverbParams,
}

impl Reverb {
    pub fn new(params: ReverbParams) -> Self {
        // Tunings clássicos Freeverb (mono) para 44.1 kHz
        let comb_tunings_44100 = [1116usize, 1188, 1277, 1356];
        let allpass_tunings_44100 = [556usize, 441usize];

        let sr = GLOBAL_CONFIG.sample_rate;
        let scale = sr / 44100.0;

        // Feedback e damping do usuário
        let room_feedback = params.room_size.clamp(0.0, 0.99);
        let damping = params.damping.clamp(0.0, 0.99);

        // Criar combs em paralelo
        let mut combs = Vec::with_capacity(comb_tunings_44100.len());
        for &t in &comb_tunings_44100 {
            let delay = ((t as f32) * scale).round().max(1.0) as usize;
            combs.push(CombFilter::new(delay, room_feedback, damping));
        }

        // Criar allpasses em série
        let mut allpasses = Vec::with_capacity(allpass_tunings_44100.len());
        for &t in &allpass_tunings_44100 {
            let delay = ((t as f32) * scale).round().max(1.0) as usize;
            allpasses.push(Allpass::new(delay, 0.5));
        }

        Self {
            combs,
            allpasses,
            params,
        }
    }
}

impl Pedal for Reverb {
    fn apply_effect(&mut self, input: i16) -> i16 {
        let input_f = input as f32;

        // Processar combs em paralelo
        let mut comb_sum = 0.0_f32;
        for comb in &mut self.combs {
            comb_sum += comb.process(input_f);
        }
        comb_sum /= self.combs.len() as f32; // normalizar

        // Processar allpasses em série
        let mut ap_out = comb_sum;
        for ap in &mut self.allpasses {
            ap_out = ap.process(ap_out);
        }

        // Mix wet/dry usando o parâmetro mix
        let wet = self.params.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;
        let out_f = input_f * dry + ap_out * wet;

        out_f.clamp(-32768.0, 32767.0) as i16
    }
}
