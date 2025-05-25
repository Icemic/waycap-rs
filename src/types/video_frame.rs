use std::os::fd::RawFd;

#[derive(Debug)]
pub struct EncodedVideoFrame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    /// Encoder value for when it should be presented (Presentation TimeStamp)
    pub pts: i64,
    /// Encoder value for when it should be decoded (Decode TimeStamp)
    pub dts: i64,
}

#[derive(Debug)]
pub struct RawVideoFrame {
    pub data: Vec<u8>,
    pub timestamp: i64,
    pub dmabuf_fd: Option<RawFd>,
    pub stride: i32,
    pub offset: u32,
    pub size: u32,
}
