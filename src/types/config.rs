#[derive(Debug)]
pub enum VideoEncoder {
    H264Nvenc,
    H264Vaapi,
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
