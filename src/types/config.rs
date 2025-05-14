#[derive(Debug)]
pub enum VideoEncoder {
    Nvenc,
    Vaapi,
}

#[derive(Debug)]
pub enum AudioEncoder {
    Opus,
}

#[derive(Debug)]
pub enum QualityPreset {
    Low,
    Medium,
    High,
    Ultra,
}
