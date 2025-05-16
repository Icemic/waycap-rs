use crate::types::error::Result;
use crate::types::video_frame::RawVideoFrame;
use crate::types::{config::QualityPreset, video_frame::EncodedVideoFrame};
use ffmpeg_next::{self as ffmpeg};
use ringbuf::HeapCons;

pub const GOP_SIZE: u32 = 30;

pub trait VideoEncoder: Send {
    fn new(width: u32, height: u32, quality: QualityPreset) -> Result<Self>
    where
        Self: Sized;
    fn process(&mut self, frame: &RawVideoFrame) -> Result<()>;
    fn drain(&mut self) -> Result<()>;
    fn reset(&mut self) -> Result<()>;
    fn drop_encoder(&mut self);
    fn get_encoder(&self) -> &Option<ffmpeg::codec::encoder::Video>;
    fn take_encoded_recv(&mut self) -> Option<HeapCons<EncodedVideoFrame>>;
}
