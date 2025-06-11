//! # waycap-rs
//!
//! `waycap-rs` is a high-level Wayland screen capture library with hardware-accelerated encoding.
//! It provides an easy-to-use API for capturing screen content on Wayland-based Linux systems,
//! using PipeWire for screen capture and hardware accelerated encoding for both video and audio.
//!
//! ## Features
//!
//! - Hardware-accelerated encoding (VAAPI and NVENC)
//! - No Copy approach to encoding video frames utilizing DMA Buffers
//! - Audio capture support
//! - Multiple quality presets
//! - Cursor visibility control
//! - Fine-grained control over capture (start, pause, resume)
//!
//! ## Platform Support
//!
//! This library currently supports Linux with Wayland display server and
//! requires the XDG Desktop Portal and PipeWire for screen capture.
//!
//! ## Example
//!
//! ```rust
//! use waycap_rs::{CaptureBuilder, QualityPreset, VideoEncoder, AudioEncoder};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create a capture instance
//!     let mut capture = CaptureBuilder::new()
//!         .with_audio()
//!         .with_quality_preset(QualityPreset::Medium)
//!         .with_cursor_shown()
//!         .with_video_encoder(VideoEncoder::Vaapi)
//!         .with_audio_encoder(AudioEncoder::Opus)
//!         .build()?;
//!     
//!     // Start capturing
//!     capture.start()?;
//!     
//!     // Get receivers for encoded frames
//!     let video_receiver = capture.take_video_receiver();
//!     let audio_receiver = capture.take_audio_receiver()?;
//!     
//!     // Process frames as needed...
//!     
//!     // Stop capturing when done
//!     capture.close()?;
//!     
//!     Ok(())
//! }
//! ```

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use capture::{audio::AudioCapture, video::VideoCapture, Terminate};
use encoders::{
    audio::AudioEncoder, nvenc_encoder::NvencEncoder, opus_encoder::OpusEncoder,
    vaapi_encoder::VaapiEncoder, video::VideoEncoder,
};
use khronos_egl::Downcast;
use pipewire::sys::_IO_wide_data;
use portal_screencast_waycap::{CursorMode, ScreenCast, SourceType};
use ringbuf::{
    traits::{Consumer, Split},
    HeapCons, HeapRb,
};
use types::{
    audio_frame::{EncodedAudioFrame, RawAudioFrame},
    config::{AudioEncoder as AudioEncoderType, QualityPreset, VideoEncoder as VideoEncoderType},
    error::{Result, WaycapError},
    video_frame::{EncodedVideoFrame, RawVideoFrame},
};
use utils::{calculate_dimensions, extract_dmabuf_planes};
use waycap_egl::EglContext;

mod capture;
mod encoders;
pub mod pipeline;
pub mod types;
mod utils;
mod waycap_egl;

/// Main capture instance for recording screen content and audio.
///
/// `Capture` provides methods to control the recording process, retrieve
/// encoded frames, and manage the capture lifecycle.
///
/// # Examples
///
/// ```
/// use waycap_rs::{CaptureBuilder, QualityPreset, VideoEncoder};
///
/// // Create a capture instance
/// let mut capture = CaptureBuilder::new()
///     .with_quality_preset(QualityPreset::Medium)
///     .with_video_encoder(VideoEncoder::Vaapi)
///     .build()
///     .expect("Failed to create capture");
///
/// // Start the capture
/// capture.start().expect("Failed to start capture");
///
/// // Get video receiver
/// let video_receiver = capture.take_video_receiver();
///
/// // Process Frames
/// while let Some(encoded_frame) = video_receiver.try_pop() {
///     println!("Received an encoded frame");
/// }
pub struct Capture {
    video_encoder: Arc<Mutex<dyn VideoEncoder + Send>>,
    audio_encoder: Option<Arc<Mutex<dyn AudioEncoder + Send>>>,
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,

    worker_handles: Vec<std::thread::JoinHandle<()>>,

    pw_video_terminate_tx: pipewire::channel::Sender<Terminate>,
    pw_audio_terminate_tx: Option<pipewire::channel::Sender<Terminate>>,
}

impl Capture {
    fn new(
        video_encoder_type: VideoEncoderType,
        audio_encoder_type: AudioEncoderType,
        quality: QualityPreset,
        include_cursor: bool,
        include_audio: bool,
        target_fps: u64,
    ) -> Result<Self> {
        let current_time = Instant::now();
        let pause = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));

        let mut join_handles: Vec<std::thread::JoinHandle<()>> = Vec::new();

        let audio_ready = Arc::new(AtomicBool::new(false));
        let video_ready = Arc::new(AtomicBool::new(false));

        let video_ring_buf = HeapRb::<RawVideoFrame>::new(120);
        let (video_ring_sender, video_ring_receiver) = video_ring_buf.split();

        let (pw_sender, pw_recv) = pipewire::channel::channel();
        let (reso_sender, reso_recv) = mpsc::channel::<(u32, u32)>();
        let video_ready_pw = Arc::clone(&video_ready);
        let audio_ready_pw = Arc::clone(&audio_ready);
        let pause_video = Arc::clone(&pause);

        let mut screen_cast = ScreenCast::new()?;
        screen_cast.set_source_types(SourceType::all());
        screen_cast.set_cursor_mode(if include_cursor {
            CursorMode::EMBEDDED
        } else {
            CursorMode::HIDDEN
        });
        let active_cast = screen_cast.start(None)?;

        let fd = active_cast.pipewire_fd();
        let stream = active_cast.streams().next().unwrap();
        let stream_node = stream.pipewire_node();

        let use_nvenc_modifiers = match video_encoder_type {
            VideoEncoderType::H264Nvenc => true,
            VideoEncoderType::H264Vaapi => false,
        };

        let pw_video_capure = std::thread::spawn(move || {
            let video_cap = VideoCapture::new(video_ready_pw, audio_ready_pw, use_nvenc_modifiers);
            video_cap
                .run(
                    fd,
                    stream_node,
                    video_ring_sender,
                    pw_recv,
                    pause_video,
                    current_time,
                    reso_sender,
                )
                .unwrap();

            let _ = active_cast.close(); // Keep this alive until the thread ends
        });

        // Wait to get back a negotiated resolution from pipewire
        let timeout = Duration::from_secs(5);
        let start = Instant::now();
        let (width, height) = loop {
            if let Ok((recv_width, recv_height)) = reso_recv.recv() {
                break (recv_width, recv_height);
            }

            if start.elapsed() > timeout {
                log::error!("Timeout waiting for PipeWire negotiated resolution.");
                std::process::exit(1);
            }

            std::thread::sleep(Duration::from_millis(10));
        };

        join_handles.push(pw_video_capure);

        let video_encoder: Arc<Mutex<dyn VideoEncoder + Send>> = match video_encoder_type {
            VideoEncoderType::H264Nvenc => {
                Arc::new(Mutex::new(NvencEncoder::new(width, height, quality)?))
            }
            VideoEncoderType::H264Vaapi => {
                Arc::new(Mutex::new(VaapiEncoder::new(width, height, quality)?))
            }
        };

        let mut audio_encoder: Option<Arc<Mutex<dyn AudioEncoder + Send>>> = None;
        let (pw_audio_sender, pw_audio_recv) = pipewire::channel::channel();
        if include_audio {
            let audio_ring_buffer = HeapRb::<RawAudioFrame>::new(10);
            let (audio_ring_sender, audio_ring_receiver) = audio_ring_buffer.split();
            let pause_capture = Arc::clone(&pause);
            let video_r = Arc::clone(&video_ready);
            let audio_r = Arc::clone(&audio_ready);
            let pw_audio_worker = std::thread::spawn(move || {
                log::debug!("Starting audio stream");
                let audio_cap = AudioCapture::new(video_r, audio_r);
                audio_cap
                    .run(
                        audio_ring_sender,
                        current_time,
                        pw_audio_recv,
                        pause_capture,
                    )
                    .unwrap();
            });
            join_handles.push(pw_audio_worker);

            let enc: Arc<Mutex<dyn AudioEncoder + Send>> = match audio_encoder_type {
                AudioEncoderType::Opus => Arc::new(Mutex::new(OpusEncoder::new()?)),
            };

            let audio_worker = audio_processor(
                Arc::clone(&enc),
                audio_ring_receiver,
                Arc::clone(&stop),
                Arc::clone(&pause),
            );
            join_handles.push(audio_worker);

            audio_encoder = Some(enc);
        } else {
            audio_ready.store(true, Ordering::Release);
        }

        let video_worker = video_processor(
            Arc::clone(&video_encoder),
            video_ring_receiver,
            Arc::clone(&stop),
            Arc::clone(&pause),
            target_fps,
            width,
            height,
        );

        join_handles.push(video_worker);

        // Wait till both threads are ready
        while !audio_ready.load(Ordering::Acquire) || !video_ready.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
        }

        log::info!("Capture started sucessfully.");

        Ok(Self {
            video_encoder,
            audio_encoder,
            stop_flag: stop,
            pause_flag: pause,
            worker_handles: join_handles,
            pw_video_terminate_tx: pw_sender,
            pw_audio_terminate_tx: Some(pw_audio_sender),
        })
    }

    /// Enables capture streams to send their frames to their encoders
    pub fn start(&mut self) -> Result<()> {
        self.pause_flag.store(false, Ordering::Release);
        Ok(())
    }

    /// Temporarily stops the recording by blocking frames from being sent to the encoders
    pub fn pause(&mut self) -> Result<()> {
        self.pause_flag.store(true, Ordering::Release);
        Ok(())
    }

    /// Stop recording and drain the encoders of any last frames they have in their internal
    /// buffers
    pub fn finish(&mut self) -> Result<()> {
        self.pause_flag.store(true, Ordering::Release);
        self.video_encoder.lock().unwrap().drain()?;
        if let Some(ref mut enc) = self.audio_encoder {
            enc.lock().unwrap().drain()?;
        }

        Ok(())
    }

    /// Resets the encoder states so we can resume encoding from within this same session
    pub fn reset(&mut self) -> Result<()> {
        self.video_encoder.lock().unwrap().reset()?;
        if let Some(ref mut enc) = self.audio_encoder {
            enc.lock().unwrap().reset()?;
        }

        Ok(())
    }

    /// Close the connection. Once called the struct cannot be re-used and must be re-built with
    /// the `CaptureBuilder` to record again.
    /// If your goal is to temporarily stop recording use [`Self::pause`] or [`Self::finish`] + [`Self::reset`]
    pub fn close(&mut self) -> Result<()> {
        self.finish()?;
        self.stop_flag.store(true, Ordering::Release);
        let _ = self.pw_video_terminate_tx.send(Terminate {});
        if let Some(pw_aud) = &self.pw_audio_terminate_tx {
            let _ = pw_aud.send(Terminate {});
        }

        for handle in self.worker_handles.drain(..) {
            let _ = handle.join();
        }

        self.video_encoder.lock().unwrap().drop_encoder();
        self.audio_encoder.take();

        Ok(())
    }

    /// Take ownership of the ring buffer which will supply you with encoded video frame data
    ///
    /// **IMPORTANT**
    ///
    /// This gives you ownership of the buffer so this can only be called *once*
    pub fn take_video_receiver(&mut self) -> HeapCons<EncodedVideoFrame> {
        self.video_encoder
            .lock()
            .unwrap()
            .take_encoded_recv()
            .unwrap()
    }

    /// Take ownership of the ring buffer which will supply you with encoded audio frame data
    ///
    /// **IMPORTANT**
    ///
    /// This gives you ownership of the buffer so this can only be called *once*
    pub fn take_audio_receiver(&mut self) -> Result<HeapCons<EncodedAudioFrame>> {
        if let Some(ref mut audio_enc) = self.audio_encoder {
            return Ok(audio_enc.lock().unwrap().take_encoded_recv().unwrap());
        } else {
            Err(WaycapError::Validation(
                "Audio encoder does not exist".to_string(),
            ))
        }
    }

    /// Perform an action with the video encoder
    /// # Examples
    ///
    /// ```
    /// let mut output = ffmpeg::format::output(&filename)?;
    ///
    /// capture.with_video_encoder(|enc| {
    ///     if let Some(video_encoder) = enc {
    ///         let mut video_stream = output.add_stream(video_encoder.codec().unwrap()).unwrap();
    ///         video_stream.set_time_base(video_encoder.time_base());
    ///         video_stream.set_parameters(video_encoder);
    ///     }
    /// });
    /// output.write_header()?;
    pub fn with_video_encoder<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Option<ffmpeg_next::encoder::Video>) -> R,
    {
        let guard = self.video_encoder.lock().unwrap();
        f(guard.get_encoder())
    }

    /// Perform an action with the audio encoder
    /// # Examples
    ///
    /// ```
    /// let mut output = ffmpeg::format::output(&filename)?;
    /// capture.with_audio_encoder(|enc| {
    ///     if let Some(audio_encoder) = enc {
    ///         let mut audio_stream = output.add_stream(audio_encoder.codec().unwrap()).unwrap();
    ///         audio_stream.set_time_base(audio_encoder.time_base());
    ///         audio_stream.set_parameters(audio_encoder);
    ///
    ///     }
    /// });
    /// output.write_header()?;
    pub fn with_audio_encoder<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Option<ffmpeg_next::encoder::Audio>) -> R,
    {
        assert!(self.audio_encoder.is_some());
        let guard = self.audio_encoder.as_ref().unwrap().lock().unwrap();
        f(guard.get_encoder())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn video_processor(
    encoder: Arc<Mutex<dyn VideoEncoder + Send>>,
    mut video_recv: HeapCons<RawVideoFrame>,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    target_fps: u64,
    width: u32,
    height: u32,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let is_nvenc = encoder.lock().unwrap().as_any().is::<NvencEncoder>();

        let egl_context = EglContext::new(width as i32, height as i32).unwrap();
        egl_context.make_current().unwrap();

        if is_nvenc {
            encoder
                .lock()
                .unwrap()
                .as_any_mut()
                .downcast_mut::<NvencEncoder>()
                .unwrap()
                .initialize_encoder()
                .unwrap();
        }

        let mut last_timestamp: u64 = 0;

        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }

            if pause.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_nanos(100));
                continue;
            }

            while let Some(raw_frame) = video_recv.try_pop() {
                let current_time = raw_frame.timestamp as u64;

                if current_time < last_timestamp + (1_000_000 / target_fps) {
                    continue;
                }

                match process_dmabuf_frame(&egl_context, &raw_frame) {
                    Ok(egl_id) => {
                        log::info!("EGL ID: {:?}", egl_id);
                        encoder
                            .lock()
                            .unwrap()
                            .process_egl_texture(egl_id, raw_frame.timestamp)
                            .unwrap();
                        last_timestamp = current_time;
                    }
                    Err(_) => {
                        log::info!("Falling back to sw encoding");
                        encoder.lock().unwrap().process(&raw_frame).unwrap();
                        last_timestamp = current_time;
                    }
                }
            }

            std::thread::sleep(Duration::from_nanos(100));
        }
    })
}

fn audio_processor(
    encoder: Arc<Mutex<dyn AudioEncoder + Send>>,
    mut audio_recv: HeapCons<RawAudioFrame>,
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        if stop.load(Ordering::Acquire) {
            break;
        }

        if pause.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_nanos(100));
            continue;
        }

        while let Some(raw_samples) = audio_recv.try_pop() {
            encoder.lock().unwrap().process(raw_samples).unwrap();
        }

        std::thread::sleep(Duration::from_nanos(100));
    })
}

fn process_dmabuf_frame(egl_ctx: &EglContext, raw_frame: &RawVideoFrame) -> Result<u32> {
    log::info!("Processing new frame");
    let dma_buf_planes = extract_dmabuf_planes(raw_frame)?;

    let format = drm_fourcc::DrmFourcc::Argb8888 as u32;
    let (width, height) = calculate_dimensions(raw_frame)?;
    let modifier = raw_frame.modifier;

    let egl_image = egl_ctx
        .create_image_from_dmabuf(&dma_buf_planes, format, width, height, modifier)
        .unwrap();

    let gl_texture_id = egl_ctx.bind_image_to_texture(egl_image).unwrap();

    // let pixels = egl_ctx
    //     .extract_pixels_from_egl_image(&egl_image, width, height)
    //     .unwrap();

    // EglContext::save_pixels_as_png(
    //     &pixels,
    //     width,
    //     height,
    //     &format!("Image-{}.png", gl_texture_id).to_string(),
    // )
    // .unwrap();

    Ok(gl_texture_id)
}
