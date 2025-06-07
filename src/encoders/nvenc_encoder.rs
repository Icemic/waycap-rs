use std::{ffi::c_void, ptr::null_mut};

use ffmpeg_next::{
    self as ffmpeg,
    ffi::{
        av_buffer_ref, av_buffer_unref, av_hwframe_ctx_init, AVHWDeviceContext, AVHWFramesContext,
        AVPixelFormat,
    },
    Rational,
};
use ringbuf::{
    traits::{Producer, Split},
    HeapCons, HeapProd, HeapRb,
};

use crate::{
    types::{
        config::QualityPreset,
        error::{Result, WaycapError},
        video_frame::{EncodedVideoFrame, RawVideoFrame},
    },
    waycap_egl::EglContext,
};

use super::{
    cuda::{
        cuCtxSetCurrent, cuGLCtxCreate, cuGraphicsGLRegisterImage, cuGraphicsMapResources,
        cuGraphicsSubResourceGetMappedArray, cuGraphicsUnmapResources,
        cuGraphicsUnregisterResource, cuMemcpy2D, AVCUDADeviceContext, CUDA_MEMCPY2D,
    },
    video::{create_hw_device, create_hw_frame_ctx, VideoEncoder, GOP_SIZE},
};

pub struct NvencEncoder {
    encoder: Option<ffmpeg::codec::encoder::Video>,
    width: u32,
    height: u32,
    encoder_name: String,
    quality: QualityPreset,
    encoded_frame_recv: Option<HeapCons<EncodedVideoFrame>>,
    encoded_frame_sender: Option<HeapProd<EncodedVideoFrame>>,
}

impl VideoEncoder for NvencEncoder {
    fn new(width: u32, height: u32, quality: QualityPreset) -> Result<Self>
    where
        Self: Sized,
    {
        let encoder_name = "h264_nvenc";
        let encoder = Self::create_encoder(width, height, encoder_name, &quality)?;
        let video_ring_buffer = HeapRb::<EncodedVideoFrame>::new(120);
        let (video_ring_sender, video_ring_receiver) = video_ring_buffer.split();

        // Self::test_gl_cuda_interop()?;

        Ok(Self {
            encoder: Some(encoder),
            width,
            height,
            encoder_name: encoder_name.to_string(),
            quality,
            encoded_frame_recv: Some(video_ring_receiver),
            encoded_frame_sender: Some(video_ring_sender),
        })
    }

    fn enable_gl_interop_on_existing_context(&mut self, egl_ctx: &EglContext) -> Result<()> {
        unsafe {
            // Make sure EGL context is current
            egl_ctx.make_current().unwrap();

            if let Some(ref mut encoder) = self.encoder {
                // Get FFmpeg's CUDA context
                let hw_device_data =
                    (*(*encoder.as_ptr()).hw_device_ctx).data as *mut AVHWDeviceContext;
                let cuda_device_ctx = (*hw_device_data).hwctx as *mut AVCUDADeviceContext;
                let ffmpeg_cuda_ctx = (*cuda_device_ctx).cuda_ctx;

                log::info!("FFmpeg created CUDA context: {:p}", ffmpeg_cuda_ctx);

                // Test if GL interop already works with this context
                let result = Self::test_gl_interop_with_context(ffmpeg_cuda_ctx);
                match result {
                    Ok(_) => {
                        log::info!("✓ GL interop already working with FFmpeg's CUDA context");
                        return Ok(());
                    }
                    Err(e) => {
                        log::warn!("GL interop not working with FFmpeg context: {}", e);
                    }
                }

                // If basic interop doesn't work, we need to do something more
                // Sometimes just setting the context current helps
                cuCtxSetCurrent(std::ptr::null_mut());
                let result = cuCtxSetCurrent(ffmpeg_cuda_ctx);
                if result != 0 {
                    return Err(format!("Failed to set FFmpeg CUDA context: {}", result).into());
                }

                // Test again
                Self::test_gl_interop_with_context(ffmpeg_cuda_ctx)?;
                log::info!("✓ GL interop enabled on FFmpeg's CUDA context");
            }
            Ok(())
        }
    }

    fn process_egl_texture(&mut self, id: u32, capture_time: i64) -> Result<()> {
        if let Some(ref mut encoder) = self.encoder {
            log::info!("Got image {:?}", id);
            let mut cuda_frame = ffmpeg::util::frame::Video::new(
                ffmpeg_next::format::Pixel::CUDA,
                encoder.width(),
                encoder.height(),
            );

            unsafe {
                // Re-use the same CUDA device as the encoder for everything
                (*cuda_frame.as_mut_ptr()).hw_frames_ctx =
                    av_buffer_ref((*encoder.as_ptr()).hw_frames_ctx);
                // let hw_device_data =
                //     (*(*encoder.as_ptr()).hw_device_ctx).data as *mut AVHWDeviceContext;
                // let cuda_device_ctx = (*hw_device_data).hwctx as *mut AVCUDADeviceContext;

                // let result = cuCtxSetCurrent(cuda_device_ctx as *mut c_void);
                //
                // if result < 0 {
                //     return Err(WaycapError::Encoding(format!(
                //         "Error setting current CUDA device: {:?}",
                //         result
                //     )));
                // }

                let mut cuda_resource: *mut c_void = null_mut();

                let result = cuGraphicsGLRegisterImage(
                    &mut cuda_resource,
                    id,
                    0x8D65, // GL_TEXTURE_2D
                    0x01,   // CU_GRAPHICS_REGISTER_FLAGS_WRITE_DISCARD
                );

                if result != 0 {
                    return Err(WaycapError::Encoding(format!(
                        "Error registering the GL image to CUDA: {:?}",
                        result
                    )));
                }

                let result =
                    cuGraphicsMapResources(1, cuda_resource as *const *mut c_void, null_mut());

                if result != 0 {
                    cuGraphicsUnregisterResource(cuda_resource);
                    return Err(WaycapError::Encoding(format!(
                        "Error mapping GL image to CUDA: {:?}",
                        result
                    )));
                }

                let mut cuda_array: *mut std::ffi::c_void = null_mut();
                let result =
                    cuGraphicsSubResourceGetMappedArray(&mut cuda_array, cuda_resource, 0, 0);

                if result != 0 {
                    cuGraphicsUnmapResources(1, cuda_resource as *const *mut c_void, null_mut());
                    cuGraphicsUnregisterResource(cuda_resource);
                    return Err(WaycapError::Encoding(format!(
                        "Error getting CUDA Array: {:?}",
                        result
                    )));
                }

                let pitch = encoder.width() * 4;
                let copy_params = CUDA_MEMCPY2D {
                    srcXInBytes: 0,
                    srcY: 0,
                    srcMemoryType: 2, //CU_MEMORYTYPE_ARRAY
                    srcHost: std::ptr::null(),
                    srcDevice: std::ptr::null(),
                    srcArray: cuda_array,
                    srcPitch: 0, // ignored for arrays

                    dstXInBytes: 0,
                    dstY: 0,
                    dstMemoryType: 1, // CU_MEMORYTYPE_DEVICE
                    dstHost: std::ptr::null_mut(),
                    dstDevice: (*cuda_frame.as_ptr()).data[0] as *mut c_void,
                    dstArray: std::ptr::null_mut(),
                    dstPitch: (*cuda_frame.as_ptr()).linesize[0] as usize,

                    WidthInBytes: pitch as usize,
                    Height: encoder.height() as usize,
                };
                let result = cuMemcpy2D(&copy_params as *const CUDA_MEMCPY2D);

                cuGraphicsUnmapResources(1, cuda_resource as *const *mut c_void, null_mut());
                cuGraphicsUnregisterResource(cuda_resource);

                if result != 0 {
                    return Err(WaycapError::Encoding(format!(
                        "Error copying CUDA array to frame: {:?}",
                        result
                    )));
                }
            }

            cuda_frame.set_pts(Some(capture_time));
            cuda_frame.set_color_range(ffmpeg_next::color::Range::MPEG);

            encoder.send_frame(&cuda_frame)?;
        }

        Ok(())
    }

    fn process(&mut self, frame: &RawVideoFrame) -> Result<()> {
        if let Some(ref mut encoder) = self.encoder {
            if let Some(fd) = frame.dmabuf_fd {
                log::info!("Received dma frame: {:?}", frame);
            } else {
                let mut src_frame = ffmpeg::util::frame::video::Video::new(
                    ffmpeg_next::format::Pixel::BGRA,
                    encoder.width(),
                    encoder.height(),
                );

                src_frame.set_pts(Some(frame.timestamp));
                src_frame.data_mut(0).copy_from_slice(&frame.data);

                encoder.send_frame(&src_frame).unwrap();
            }

            // Retrieve and handle the encoded packet
            let mut packet = ffmpeg::codec::packet::Packet::empty();
            if encoder.receive_packet(&mut packet).is_ok() {
                if let Some(data) = packet.data() {
                    if let Some(ref mut sender) = self.encoded_frame_sender {
                        if sender
                            .try_push(EncodedVideoFrame {
                                data: data.to_vec(),
                                is_keyframe: packet.is_key(),
                                pts: packet.pts().unwrap_or(0),
                                dts: packet.dts().unwrap_or(0),
                            })
                            .is_err()
                        {
                            log::error!("Could not send encoded packet to the ringbuf");
                        }
                    }
                };
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        if let Some(ref mut encoder) = self.encoder {
            // Drain encoder
            encoder.send_eof()?;
            let mut packet = ffmpeg::codec::packet::Packet::empty();
            while encoder.receive_packet(&mut packet).is_ok() {
                if let Some(data) = packet.data() {
                    if let Some(ref mut sender) = self.encoded_frame_sender {
                        if sender
                            .try_push(EncodedVideoFrame {
                                data: data.to_vec(),
                                is_keyframe: packet.is_key(),
                                pts: packet.pts().unwrap_or(0),
                                dts: packet.dts().unwrap_or(0),
                            })
                            .is_err()
                        {
                            log::error!("Could not send encoded packet to the ringbuf");
                        }
                    }
                };
                packet = ffmpeg::codec::packet::Packet::empty();
            }
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.drop_encoder();
        let new_encoder =
            Self::create_encoder(self.width, self.height, &self.encoder_name, &self.quality)?;

        self.encoder = Some(new_encoder);
        Ok(())
    }

    fn get_encoder(&self) -> &Option<ffmpeg::codec::encoder::Video> {
        &self.encoder
    }

    fn drop_encoder(&mut self) {
        self.encoder.take();
    }

    fn take_encoded_recv(&mut self) -> Option<HeapCons<EncodedVideoFrame>> {
        self.encoded_frame_recv.take()
    }
}

impl NvencEncoder {
    fn create_encoder(
        width: u32,
        height: u32,
        encoder: &str,
        quality: &QualityPreset,
    ) -> Result<ffmpeg::codec::encoder::Video> {
        let encoder_codec =
            ffmpeg::codec::encoder::find_by_name(encoder).ok_or(ffmpeg::Error::EncoderNotFound)?;

        let mut encoder_ctx = ffmpeg::codec::context::Context::new_with_codec(encoder_codec)
            .encoder()
            .video()?;

        encoder_ctx.set_width(width);
        encoder_ctx.set_height(height);
        encoder_ctx.set_format(ffmpeg::format::Pixel::CUDA);
        encoder_ctx.set_bit_rate(16_000_000);

        let mut nvenc_device =
            create_hw_device(ffmpeg_next::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA)?;
        let mut frame_ctx = create_hw_frame_ctx(nvenc_device)?;

        unsafe {
            let hw_frame_context = &mut *((*frame_ctx).data as *mut AVHWFramesContext);

            hw_frame_context.width = width as i32;
            hw_frame_context.height = height as i32;
            hw_frame_context.sw_format = AVPixelFormat::AV_PIX_FMT_BGRA;
            hw_frame_context.format = encoder_ctx.format().into();
            hw_frame_context.device_ref = av_buffer_ref(nvenc_device);
            hw_frame_context.device_ctx = (*nvenc_device).data as *mut AVHWDeviceContext;
            // Decides buffer size if we do not pop frame from the encoder we cannot
            // enqueue more than these many -- maybe adjust but for now setting it to
            // doble target fps
            hw_frame_context.initial_pool_size = 120;

            let err = av_hwframe_ctx_init(frame_ctx);
            if err < 0 {
                return Err(WaycapError::Init(format!(
                    "Error trying to initialize hw frame context: {:?}",
                    err
                )));
            }

            (*encoder_ctx.as_mut_ptr()).hw_device_ctx = av_buffer_ref(nvenc_device);
            (*encoder_ctx.as_mut_ptr()).hw_frames_ctx = av_buffer_ref(frame_ctx);

            av_buffer_unref(&mut nvenc_device);
            av_buffer_unref(&mut frame_ctx);
        }

        encoder_ctx.set_time_base(Rational::new(1, 1_000_000));
        encoder_ctx.set_gop(GOP_SIZE);

        let encoder_params = ffmpeg::codec::Parameters::new();

        let opts = Self::get_encoder_params(quality);

        encoder_ctx.set_parameters(encoder_params)?;
        let encoder = encoder_ctx.open_with(opts)?;

        Ok(encoder)
    }

    fn get_encoder_params(quality: &QualityPreset) -> ffmpeg::Dictionary {
        let mut opts = ffmpeg::Dictionary::new();
        opts.set("vsync", "vfr");
        opts.set("rc", "vbr");
        opts.set("tune", "hq");
        match quality {
            QualityPreset::Low => {
                opts.set("preset", "p2");
                opts.set("cq", "30");
                opts.set("b:v", "20M");
            }
            QualityPreset::Medium => {
                opts.set("preset", "p4");
                opts.set("cq", "25");
                opts.set("b:v", "40M");
            }
            QualityPreset::High => {
                opts.set("preset", "p7");
                opts.set("cq", "20");
                opts.set("b:v", "80M");
            }
            QualityPreset::Ultra => {
                opts.set("preset", "p7");
                opts.set("cq", "15");
                opts.set("b:v", "120M");
            }
        }
        opts
    }

    fn test_gl_cuda_interop() -> Result<()> {
        unsafe {
            // Create a small test texture
            let mut test_texture = 0;
            gl::GenTextures(1, &mut test_texture);
            gl::BindTexture(gl::TEXTURE_2D, test_texture);

            // Create minimal texture data
            let test_data = vec![255u8; 64 * 64 * 4]; // 64x64 RGBA
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                64,
                64,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                test_data.as_ptr() as *const std::ffi::c_void,
            );

            let gl_error = gl::GetError();
            if gl_error != gl::NO_ERROR {
                gl::DeleteTextures(1, &test_texture);
                return Err(format!("GL error creating test texture: 0x{:x}", gl_error).into());
            }

            // Try to register with CUDA
            let mut cuda_resource: *mut std::ffi::c_void = std::ptr::null_mut();
            let result = cuGraphicsGLRegisterImage(
                &mut cuda_resource,
                test_texture,
                0x0DE1, // GL_TEXTURE_2D
                0x02,   // CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY
            );

            if result == 0 {
                log::info!("✓ GL/CUDA interop test successful!");
                cuGraphicsUnregisterResource(cuda_resource);
            } else {
                log::error!("✗ GL/CUDA interop test failed with error: {}", result);
                gl::DeleteTextures(1, &test_texture);
                return Err(format!("GL/CUDA interop not working: {}", result).into());
            }

            gl::DeleteTextures(1, &test_texture);
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        Ok(())
    }

    fn test_gl_interop_with_context(cuda_ctx: *mut std::ffi::c_void) -> Result<()> {
        unsafe {
            // Clear any current context first
            cuCtxSetCurrent(std::ptr::null_mut());

            // Set the context we want to test
            let result = cuCtxSetCurrent(cuda_ctx);
            if result != 0 {
                return Err(format!("Failed to set context: {}", result).into());
            }

            // Create test texture
            let mut test_texture = 0;
            gl::GenTextures(1, &mut test_texture);
            gl::BindTexture(gl::TEXTURE_2D, test_texture);

            let test_data = vec![255u8; 32 * 32 * 4];
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                32,
                32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                test_data.as_ptr() as *const std::ffi::c_void,
            );

            // Try to register
            let mut cuda_resource: *mut std::ffi::c_void = std::ptr::null_mut();
            let result = cuGraphicsGLRegisterImage(
                &mut cuda_resource,
                test_texture,
                0x0DE1, // GL_TEXTURE_2D
                0x02,   // READ_ONLY
            );

            // Cleanup
            gl::DeleteTextures(1, &test_texture);
            gl::BindTexture(gl::TEXTURE_2D, 0);

            if result == 0 {
                cuGraphicsUnregisterResource(cuda_resource);
                Ok(())
            } else {
                Err(format!("GL interop test failed: {}", result).into())
            }
        }
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        if let Err(e) = self.drain() {
            log::error!("Error while draining nvenc encoder during drop: {:?}", e);
        }
        self.drop_encoder();
    }
}
