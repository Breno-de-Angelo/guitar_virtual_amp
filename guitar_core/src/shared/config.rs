pub struct GlobalConfig {
    pub sample_rate: f32,
    pub period_size: usize,
    /// Default host-callback buffer size in frames, used at startup before any
    /// user preference is known. This is only a starting point: the actual
    /// buffer size used by a running stream is chosen at runtime (see
    /// `backend::capture`'s `buffer_frames` parameters) and can be tuned live
    /// from the UI's "Buffer Size" control, persisted via
    /// `shared::settings::AppSettings::buffer_frames`.
    pub buffer_size: usize,
}

pub const GLOBAL_CONFIG: GlobalConfig = GlobalConfig {
    sample_rate: 48000.0,
    period_size: 256,
    buffer_size: 1024,
};

/// Buffer sizes (in frames) offered by the UI's buffer-size selector, from
/// lowest-latency/highest-risk-of-underrun to safest/highest-latency. Not
/// every device supports every size -- `backend::capture::resolve_stream_config`
/// clamps whatever is requested into the device's actual supported range (or
/// falls back to the device default if the range can't be queried).
pub const BUFFER_SIZE_OPTIONS_FRAMES: &[u32] = &[32, 64, 128, 256, 512, 1024, 2048, 4096];
