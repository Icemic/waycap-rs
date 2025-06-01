use std::{
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

use ringbuf::traits::Consumer;
use waycap_rs::{
    pipeline::builder::CaptureBuilder,
    types::{
        config::{AudioEncoder, QualityPreset, VideoEncoder},
        error::WaycapError,
    },
};

fn main() -> Result<(), WaycapError> {
    simple_logging::log_to_stderr(log::LevelFilter::Info);
    log::info!("Simple Capture Example");
    log::info!("=====================");
    log::info!("This example will capture your screen");
    log::info!("and print the frame information to the console");
    log::info!("");
    log::info!("Press Enter to start...");

    // Wait for user input
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let stop = Arc::new(AtomicBool::new(false));

    let mut capture = CaptureBuilder::new()
        .with_quality_preset(QualityPreset::Medium)
        .with_cursor_shown()
        .with_video_encoder(VideoEncoder::H264Nvenc)
        .build()?;

    capture.start()?;

    let mut video_recv = capture.take_video_receiver();
    // let mut audio_recv = capture.take_audio_receiver().unwrap();

    let ctrlc_clone = Arc::clone(&stop);
    ctrlc::set_handler(move || {
        println!("Stopping...");
        capture.close().unwrap();
        ctrlc_clone.store(true, std::sync::atomic::Ordering::Relaxed);
    })
    .unwrap();

    let h1stop = Arc::clone(&stop);
    let handle1 = std::thread::spawn(move || loop {
        if h1stop.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }

        while let Some(encoded_frame) = video_recv.try_pop() {
            log::info!("======= NEW VIDEO FRAME =======");
            log::info!("Is Key Frame: {:?}", encoded_frame.is_keyframe);
            log::info!("PTS: {:?}", encoded_frame.pts);
            log::info!("DTS: {:?}", encoded_frame.dts);
            log::info!("===============================");
        }
        std::thread::sleep(Duration::from_nanos(100));
    });

    // let h2stop = Arc::clone(&stop);
    // let handle2 = std::thread::spawn(move || loop {
    //     if h2stop.load(std::sync::atomic::Ordering::Acquire) {
    //         break;
    //     }
    //
    //     while let Some(encoded_frame) = audio_recv.try_pop() {
    //         log::info!("======= NEW AUDIO FRAME =======");
    //         log::info!("PTS: {:?}", encoded_frame.pts);
    //         log::info!("Capture Time Stamp: {:?}", encoded_frame.timestamp);
    //         log::info!("===============================");
    //     }
    //
    //     std::thread::sleep(Duration::from_nanos(100));
    // });

    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = handle1.join();
    // let _ = handle2.join();

    Ok(())
}
