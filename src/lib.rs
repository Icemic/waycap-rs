use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use capture::{audio::AudioCapture, video::VideoCapture, Terminate};
use encoders::{
    audio::AudioEncoder, opus_encoder::OpusEncoder, vaapi_encoder::VaapiEncoder,
    video::VideoEncoder,
};
use portal_screencast::{ScreenCast, SourceType};
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

mod capture;
mod encoders;
pub mod pipeline;
pub mod types;

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
        let active_cast = screen_cast.start(None)?;

        let fd = active_cast.pipewire_fd();
        let stream = active_cast.streams().next().unwrap();
        let stream_node = stream.pipewire_node();

        let pw_video_capure = std::thread::spawn(move || {
            let video_cap = VideoCapture::new(video_ready_pw, audio_ready_pw);
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
        let (mut width, mut height) = (0, 0);
        loop {
            if let Ok((recv_width, recv_height)) = reso_recv.recv() {
                (width, height) = (recv_width, recv_height);
                break;
            }

            if start.elapsed() > timeout {
                log::error!("Timeout waiting for PipeWire negotiated resolution.");
                std::process::exit(1);
            }

            std::thread::sleep(Duration::from_millis(10));
        }
        join_handles.push(pw_video_capure);

        let video_encoder: Arc<Mutex<dyn VideoEncoder + Send>> = match video_encoder_type {
            VideoEncoderType::Nvenc => {
                return Err(WaycapError::Init(
                    "Nvenc is not yet implemented.".to_string(),
                ))
            }
            VideoEncoderType::Vaapi => {
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
        );
        join_handles.push(video_worker);

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

    pub fn start(&mut self) -> Result<()> {
        self.pause_flag.store(false, Ordering::Release);
        Ok(())
    }

    pub fn pause(&mut self) -> Result<()> {
        self.pause_flag.store(true, Ordering::Release);
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()> {
        self.pause_flag.store(true, Ordering::Release);
        self.video_encoder.lock().unwrap().drain()?;
        if let Some(ref mut enc) = self.audio_encoder {
            enc.lock().unwrap().drain()?;
        }

        Ok(())
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.finish()?;
        self.stop_flag.store(true, Ordering::Release);
        Ok(())
    }

    pub fn take_video_receiver(&mut self) -> HeapCons<EncodedVideoFrame> {
        self.video_encoder
            .lock()
            .unwrap()
            .take_encoded_recv()
            .unwrap()
    }

    pub fn take_audio_receiver(&mut self) -> Result<HeapCons<EncodedAudioFrame>> {
        if let Some(ref mut audio_enc) = self.audio_encoder {
            return Ok(audio_enc.lock().unwrap().take_encoded_recv().unwrap());
        } else {
            return Err(WaycapError::Validation(
                "Audio encoder does not exist".to_string(),
            ));
        }
    }

    pub fn with_video_encoder<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Option<ffmpeg_next::encoder::Video>) -> R,
    {
        let guard = self.video_encoder.lock().unwrap();
        f(guard.get_encoder())
    }

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
        let _ = self.finalize();
        self.stop_flag.store(true, Ordering::Release);

        let _ = self.pw_video_terminate_tx.send(Terminate {});
        if let Some(pw_aud) = &self.pw_audio_terminate_tx {
            let _ = pw_aud.send(Terminate {});
        }

        for handle in self.worker_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn video_processor(
    encoder: Arc<Mutex<dyn VideoEncoder + Send>>,
    mut video_recv: HeapCons<RawVideoFrame>,
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

        while let Some(raw_frame) = video_recv.try_pop() {
            encoder.lock().unwrap().process(&raw_frame).unwrap();
        }

        std::thread::sleep(Duration::from_nanos(100));
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
