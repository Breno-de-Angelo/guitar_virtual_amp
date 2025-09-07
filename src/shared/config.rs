pub struct GlobalConfig {
    pub sample_rate: f32,
    pub period_size: usize,
    pub buffer_size: usize,
}

pub const GLOBAL_CONFIG: GlobalConfig = GlobalConfig {
    sample_rate: 48000.0,
    period_size: 256,
    buffer_size: 1024,
};
