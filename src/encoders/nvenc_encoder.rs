use std::{ffi::c_void, ptr::null_mut};

use ffmpeg_next::{
    self as ffmpeg,
    ffi::{
        av_buffer_ref, av_buffer_unref, av_hwframe_ctx_init, av_hwframe_get_buffer,
        AVHWDeviceContext, AVHWFramesContext, AVPixelFormat,
    },
    Rational,
};
use ringbuf::{
    traits::{Producer, Split},
    HeapCons, HeapProd, HeapRb,
};

use crate::{
    encoders::cuda::{
        cuArrayGetDescriptor, cuCtxGetCurrent, cuGraphicsResourceSetMapFlags, CUarray_format,
        CUmemorytype, CUDA_ARRAY_DESCRIPTOR, CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE,
    },
    types::{
        config::QualityPreset,
        error::{Result, WaycapError},
        video_frame::{EncodedVideoFrame, RawVideoFrame},
    },
    waycap_egl::EglContext,
};

use super::{
    cuda::{
        cuCtxSetCurrent, cuGraphicsGLRegisterImage, cuGraphicsMapResources,
        cuGraphicsSubResourceGetMappedArray, cuGraphicsUnmapResources,
        cuGraphicsUnregisterResource, cuMemcpy2D, AVCUDADeviceContext, CUDA_MEMCPY2D,
    },
    video::{
        create_hw_device, create_hw_device_with_opts, create_hw_frame_ctx, VideoEncoder, GOP_SIZE,
    },
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

        Self::test_gl_cuda_interop()?;

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

    fn process_egl_texture(&mut self, id: u32, capture_time: i64) -> Result<()> {
        if let Some(ref mut encoder) = self.encoder {
            log::info!("Got texture {}", id);

            let mut cuda_frame = ffmpeg::util::frame::Video::new(
                ffmpeg_next::format::Pixel::CUDA,
                encoder.width(),
                encoder.height(),
            );

            unsafe {
                // Set up CUDA context from encoder
                let hw_device_data =
                    (*(*encoder.as_ptr()).hw_device_ctx).data as *mut AVHWDeviceContext;

                let ret = av_hwframe_get_buffer(
                    (*encoder.as_ptr()).hw_frames_ctx,
                    cuda_frame.as_mut_ptr(),
                    0,
                );
                if ret < 0 {
                    return Err(WaycapError::Encoding(format!(
                        "Failed to allocate CUDA frame buffer: {}",
                        ret
                    )));
                }

                let cuda_device_ctx = (*hw_device_data).hwctx as *mut AVCUDADeviceContext;
                let encoder_cuda_ctx = (*cuda_device_ctx).cuda_ctx;

                // Check if context is valid before using it
                if encoder_cuda_ctx.is_null() {
                    return Err(WaycapError::Encoding("CUDA context is null".into()));
                }

                log::info!("Encoder CUDA context: {:p}", encoder_cuda_ctx);

                // Check current context
                let mut current_ctx: *mut std::ffi::c_void = std::ptr::null_mut();
                let result = cuCtxGetCurrent(&mut current_ctx);
                if result != 0 {
                    return Err(WaycapError::Encoding(format!(
                        "Failed to get current CUDA context: {}",
                        result
                    )));
                }

                log::info!("Current CUDA context: {:p}", current_ctx);

                // If no context is current or different context, set the encoder's context
                if current_ctx != encoder_cuda_ctx {
                    let result = cuCtxSetCurrent(encoder_cuda_ctx);
                    if result != 0 {
                        return Err(WaycapError::Encoding(format!(
                            "Failed to set encoder CUDA context: {} (context may be destroyed)",
                            result
                        )));
                    }
                    log::info!("Set encoder context as current");

                    // Verify context is actually current now
                    cuCtxGetCurrent(&mut current_ctx);
                    if current_ctx != encoder_cuda_ctx {
                        return Err(WaycapError::Encoding(
                            "Failed to make encoder context current".into(),
                        ));
                    }
                }

                // Try to register GL texture with CUDA
                let mut cuda_resource: *mut c_void = null_mut();
                let result = cuGraphicsGLRegisterImage(
                    &mut cuda_resource,
                    id,
                    gl::TEXTURE_2D, // GL_TEXTURE_2D
                    0x00,           // CU_GRAPHICS_REGISTER_FLAGS_READ_NONE
                );

                if result != 0 {
                    let error_msg = match result {
                        709 => "Context destroyed or not initialized",
                        1 => "Invalid value",
                        201 => "Context already current",
                        205 => "Invalid graphics context",
                        _ => "Unknown error",
                    };
                    return Err(WaycapError::Encoding(format!(
                        "Error registering GL texture to CUDA: {} ({}) - {}",
                        result, result, error_msg
                    )));
                }

                log::info!("Successfully registered texture {}", id);

                let result = cuGraphicsResourceSetMapFlags(
                    cuda_resource,
                    CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE,
                );

                if result != 0 {
                    log::error!("cuGraphicsResourceSetMapFlags failed: {}", result);
                    cuGraphicsUnregisterResource(cuda_resource);
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                    return Err(WaycapError::Encoding(format!(
                        "Failed to set graphics resource map flags: {}",
                        result
                    )));
                }

                log::info!("✓ Set graphics resource map flags");

                let result = cuGraphicsMapResources(1, &cuda_resource, null_mut());
                if result != 0 {
                    cuGraphicsUnregisterResource(cuda_resource);
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                    return Err(WaycapError::Encoding(format!(
                        "Error mapping GL image to CUDA: {}",
                        result
                    )));
                }

                let mut cuda_array: *mut std::ffi::c_void = null_mut();
                let result =
                    cuGraphicsSubResourceGetMappedArray(&mut cuda_array, cuda_resource, 0, 0);
                if result != 0 {
                    cuGraphicsUnmapResources(1, &cuda_resource, null_mut());
                    cuGraphicsUnregisterResource(cuda_resource);
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                    return Err(WaycapError::Encoding(format!(
                        "Error getting CUDA Array: {}",
                        result
                    )));
                }

                let copy_params = CUDA_MEMCPY2D {
                    srcXInBytes: 0,
                    srcY: 0,
                    srcMemoryType: CUmemorytype::CU_MEMORYTYPE_ARRAY as u32,
                    srcHost: std::ptr::null(),
                    srcDevice: 0,
                    srcArray: cuda_array,
                    srcPitch: encoder.width() as usize, // ← C code uses frame->width, NOT 0!

                    dstXInBytes: 0,
                    dstY: 0,
                    dstMemoryType: CUmemorytype::CU_MEMORYTYPE_DEVICE as u32,
                    dstHost: std::ptr::null_mut(),
                    dstDevice: (*cuda_frame.as_ptr()).data[0] as u64,
                    dstArray: std::ptr::null_mut(),
                    dstPitch: (*cuda_frame.as_ptr()).linesize[0] as usize,

                    WidthInBytes: encoder.width() as usize, // ← C code uses frame->width, NOT width*4!
                    Height: encoder.height() as usize,
                };

                log::info!("Copy parameters (matching C code):");
                log::info!("  srcPitch: {} (C: frame->width)", copy_params.srcPitch);
                log::info!(
                    "  dstPitch: {} (C: frame->linesize[0])",
                    copy_params.dstPitch
                );
                log::info!(
                    "  WidthInBytes: {} (C: frame->width)",
                    copy_params.WidthInBytes
                );
                log::info!("  Height: {}", copy_params.Height);

                let mut current_ctx: *mut std::ffi::c_void = std::ptr::null_mut();
                cuCtxGetCurrent(&mut current_ctx);
                log::info!("Current context before cuMemcpy2D: {:p}", current_ctx);

                let result = cuMemcpy2D(&copy_params);
                log::info!("cuMemcpy2D with C-style params result: {}", result);

                // Cleanup
                cuGraphicsUnmapResources(1, &cuda_resource, null_mut());
                cuGraphicsUnregisterResource(cuda_resource);
                gl::BindTexture(gl::TEXTURE_2D, 0);

                if result != 0 {
                    return Err(WaycapError::Encoding(format!(
                        "Error copying CUDA array to frame: {}",
                        result
                    )));
                }

                log::info!("Successfully copied texture {} to CUDA frame", id);
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

        let mut opts = ffmpeg::Dictionary::new();

        // Reuse the same context as EGL
        // to allow open gl interop
        opts.set("primary_ctx", "1");
        let mut nvenc_device = create_hw_device_with_opts(
            ffmpeg_next::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            opts,
        )?;

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

            let hw_devc = (*nvenc_device).data as *mut AVHWDeviceContext;
            let nvenc_dvc = (*hw_devc).hwctx as *mut AVCUDADeviceContext;

            if (*nvenc_dvc).cuda_ctx.is_null() {
                return Err(WaycapError::Init("FFmpeg CUDA context is null".to_string()));
            }

            let result = cuCtxSetCurrent((*nvenc_dvc).cuda_ctx);
            if result != 0 {
                return Err(WaycapError::Init(format!(
                    "Failed to set FFmpeg CUDA context current: {}",
                    result
                )));
            }

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
            let mut current_context = std::ptr::null_mut();
            let result = cuCtxGetCurrent(&mut current_context);
            if result != 0 || current_context.is_null() {
                return Err(format!(
                    "No CUDA context is current after encoder creation, {:?}",
                    result
                )
                .into());
            }

            log::info!("✓ CUDA context exists after encoder creation");
            let gl_error = gl::GetError();

            if gl_error != gl::NO_ERROR {
                return Err(
                    format!("OpenGL error before CUDA registration: 0x{:x}", gl_error).into(),
                );
            }
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

            let gl_error = gl::GetError();
            if gl_error != gl::NO_ERROR {
                return Err(
                    format!("OpenGL error before CUDA registration: 0x{:x}", gl_error).into(),
                );
            }
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
