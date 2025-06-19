# waycap-rs

A high-level Wayland screen capture library with hardware-accelerated encoding for Linux environments.

## Features

- **Hardware-accelerated video encoding** (Using VAAPI or NVENC)
- **Audio capture** with Opus encoding
- **Copy-Free** video encoding leveraging pipewire's DMA Buffers
- **Multiple quality presets** for various use cases
- **Cursor visibility control**
- **Simple, ergonomic API** for easy integration

## Requirements

- Linux with Wayland display server
- XDG Desktop Portal
- PipeWire
- VA-API compatible hardware for VAAPI encoding
- CUDA compatible hardware for NVENC encoding

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
waycap-rs = "0.2.0"
```

## Examples Usage
```rust
use waycap_rs::{CaptureBuilder, QualityPreset, VideoEncoder, AudioEncoder};
use std::{thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a capture session
    let mut capture = CaptureBuilder::new()
        .with_audio()
        .with_quality_preset(QualityPreset::Medium)
        .with_cursor_shown()
        .with_video_encoder(VideoEncoder::Vaapi)
        .with_audio_encoder(AudioEncoder::Opus)
        .build()?;
    
    // Start capturing
    capture.start()?;
    
    // Get receivers for encoded frames
    let video_receiver = capture.take_video_receiver();
    let audio_receiver = capture.take_audio_receiver()?;
    
    // Process frames in separate threads
    let video_thread = thread::spawn(move || {
        while let Some(frame) = video_receiver.try_pop() {
            // Process video frame (e.g., save to file, stream, etc.)
            println!("Video frame: keyframe={}, size={}", frame.is_keyframe, frame.data.len());
        }
    });
    
    let audio_thread = thread::spawn(move || {
        while let Some(frame) = audio_receiver.try_pop() {
            // Process audio frame
            println!("Audio frame: size={}", frame.data.len());
        }
    });
    
    // Capture for 10 seconds
    thread::sleep(Duration::from_secs(10));
    
    // Stop capturing
    capture.close()?;
    
    // Wait for threads to finish
    video_thread.join().unwrap();
    audio_thread.join().unwrap();
    
    Ok(())
}
```

## Primary Use Case: [WayCap](https://github.com/Adonca2203/WayCap)
This library was created primarily to support the development of WayCap -- a low-latency screen recorder targetting Wayland Linux DEs.
waycap-rs originally lived within this application but was broken out to split library and application logic, you can read more about
that project over at its github page

https://github.com/Adonca2203/WayCap

## Contributing
Contributions are always welcome and encouraged, feel free to open a PR with any features you think may be missing.

### Currently I have planned adding the following:
- Capturing more than system audio -- Support for microphones

### Areas for Improvement aside from the things already mentioned:
- Any optimizations for the library's core capture logic.
- Documentation around the public facing APIs.
- Bug Reports via github Issues
- Platform Testing as I am currently limited by my hardware
- Leverage the GpuVendor field of EGL to dynamically set the target encoder

## Pull Request Guidelines
- **Fork the repository** based off the `main` branch.
- **Write clear and well documented** with comments where appropriate.
- **Unit Tests** if applicable.
- **Code Examples** in `/examples` if applicable.
- **Include references to issues** if applicable.
