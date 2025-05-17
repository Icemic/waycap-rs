use std::{ffi::CString, ptr::null_mut};

use ffmpeg_next::{
    self as ffmpeg,
    ffi::{
        av_buffer_ref, av_buffer_unref, av_hwdevice_ctx_create, av_hwframe_ctx_alloc,
        av_hwframe_ctx_init, AVBufferRef, AVHWDeviceContext, AVHWFramesContext, AVPixelFormat,
    },
    Rational,
};
use ringbuf::{
    traits::{Producer, Split},
    HeapCons, HeapProd, HeapRb,
};

use crate::types::{
    config::QualityPreset,
    error::{Result, WaycapError},
    video_frame::{EncodedVideoFrame, RawVideoFrame},
};

use super::video::{VideoEncoder, GOP_SIZE};

pub struct NvencEncoder {
    encoder: Option<ffmpeg::codec::encoder::Video>,
    width: u32,
    height: u32,
    encoder_name: String,
    quality: QualityPreset,
    encoded_frame_recv: Option<HeapCons<EncodedVideoFrame>>,
    encoded_frame_sender: Option<HeapProd<EncodedVideoFrame>>,
    filter_graph: Option<ffmpeg::filter::Graph>,
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
        let filter_graph = Some(Self::create_filter_graph(&encoder, width, height)?);

        Ok(Self {
            encoder: Some(encoder),
            width,
            height,
            encoder_name: encoder_name.to_string(),
            quality,
            encoded_frame_recv: Some(video_ring_receiver),
            encoded_frame_sender: Some(video_ring_sender),
            filter_graph,
        })
    }

    fn process(&mut self, frame: &RawVideoFrame) -> Result<()> {
        if let Some(ref mut encoder) = self.encoder {
            if let Some(fd) = frame.dmabuf_fd {
                let mut cuda_frame = ffmpeg::util::frame::Video::new(
                    ffmpeg_next::format::Pixel::CUDA,
                    encoder.width(),
                    encoder.height(),
                );

                unsafe {
                    (*cuda_frame.as_mut_ptr()).hw_frames_ctx =
                        av_buffer_ref((*encoder.as_ptr()).hw_frames_ctx);

                }

                cuda_frame.set_pts(Some(frame.timestamp));

                // Send the frame to the filter graph for processing
                self.filter_graph
                    .as_mut()
                    .unwrap()
                    .get("in")
                    .unwrap()
                    .source()
                    .add(&cuda_frame)
                    .unwrap();

                let mut filtered = ffmpeg::util::frame::Video::empty();
                if self
                    .filter_graph
                    .as_mut()
                    .unwrap()
                    .get("out")
                    .unwrap()
                    .sink()
                    .frame(&mut filtered)
                    .is_ok()
                {
                    encoder.send_frame(&filtered)?;
                }
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
            // Drain the filter graph
            let mut filtered = ffmpeg::util::frame::Video::empty();
            while self
                .filter_graph
                .as_mut()
                .unwrap()
                .get("out")
                .unwrap()
                .sink()
                .frame(&mut filtered)
                .is_ok()
            {
                encoder.send_frame(&filtered)?;
            }

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

        let new_filter_graph = Self::create_filter_graph(&new_encoder, self.width, self.height)?;

        self.encoder = Some(new_encoder);
        self.filter_graph = Some(new_filter_graph);
        Ok(())
    }

    fn get_encoder(&self) -> &Option<ffmpeg::codec::encoder::Video> {
        &self.encoder
    }

    fn drop_encoder(&mut self) {
        self.encoder.take();
        self.filter_graph.take();
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

        // Create CUDA device and context
        let mut cuda_device = Self::create_cuda_device()?;
        let mut frame_ctx = Self::create_cuda_frame_ctx(cuda_device)?;

        unsafe {
            let hw_frame_context = &mut *((*frame_ctx).data as *mut AVHWFramesContext);
            hw_frame_context.width = width as i32;
            hw_frame_context.height = height as i32;
            hw_frame_context.sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            hw_frame_context.format = encoder_ctx.format().into();
            hw_frame_context.device_ref = av_buffer_ref(cuda_device);
            hw_frame_context.device_ctx = (*cuda_device).data as *mut AVHWDeviceContext;
            hw_frame_context.initial_pool_size = 120;

            let err = av_hwframe_ctx_init(frame_ctx);
            if err < 0 {
                return Err(WaycapError::Init(format!(
                    "Error trying to initialize hw frame context: {:?}",
                    err
                )));
            }

            (*encoder_ctx.as_mut_ptr()).hw_device_ctx = av_buffer_ref(cuda_device);
            (*encoder_ctx.as_mut_ptr()).hw_frames_ctx = av_buffer_ref(frame_ctx);

            av_buffer_unref(&mut cuda_device);
            av_buffer_unref(&mut frame_ctx);
        }

        // Set time base and GOP size
        encoder_ctx.set_time_base(Rational::new(1, 1_000_000));
        encoder_ctx.set_gop(GOP_SIZE);

        let encoder_params = ffmpeg::codec::Parameters::new();
        let opts = Self::get_encoder_params(quality);

        encoder_ctx.set_parameters(encoder_params)?;
        let encoder = encoder_ctx.open_with(opts)?;
        Ok(encoder)
    }

    fn create_cuda_frame_ctx(device: *mut AVBufferRef) -> Result<*mut AVBufferRef> {
        unsafe {
            let frame = av_hwframe_ctx_alloc(device);

            if frame.is_null() {
                return Err(WaycapError::Init(
                    "Could not create CUDA frame context".to_string(),
                ));
            }

            Ok(frame)
        }
    }

    fn create_cuda_device() -> Result<*mut AVBufferRef> {
        unsafe {
            let mut device: *mut AVBufferRef = null_mut();
            // On Linux, you can specify the device ID (0 for the first GPU)
            let device_path = CString::new("0").unwrap();
            let ret = av_hwdevice_ctx_create(
                &mut device,
                ffmpeg_next::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                device_path.as_ptr(),
                null_mut(),
                0,
            );
            if ret < 0 {
                return Err(WaycapError::Init(format!(
                    "Failed to create CUDA device: Error code {}",
                    ret
                )));
            }

            Ok(device)
        }
    }

    fn get_encoder_params(quality: &QualityPreset) -> ffmpeg::Dictionary {
        let mut opts = ffmpeg::Dictionary::new();
        opts.set("vsync", "vfr");

        // NVENC specific options
        opts.set("preset", "p4"); // This is equivalent to "medium" - adjust as needed

        // Configure quality parameters based on preset
        match quality {
            QualityPreset::Low => {
                opts.set("rc", "vbr"); // Variable bitrate
                opts.set("cq", "30"); // Higher CQ value = lower quality
                opts.set("qmin", "25");
                opts.set("qmax", "35");
            }
            QualityPreset::Medium => {
                opts.set("rc", "vbr");
                opts.set("cq", "23");
                opts.set("qmin", "18");
                opts.set("qmax", "28");
            }
            QualityPreset::High => {
                opts.set("rc", "vbr");
                opts.set("cq", "18");
                opts.set("qmin", "13");
                opts.set("qmax", "23");
            }
            QualityPreset::Ultra => {
                opts.set("rc", "vbr");
                opts.set("cq", "10");
                opts.set("qmin", "5");
                opts.set("qmax", "15");
                opts.set("spatial-aq", "1"); // Spatial adaptive quantization
                opts.set("temporal-aq", "1"); // Temporal adaptive quantization
            }
        }

        // Additional NVENC specific options
        opts.set("zerolatency", "1"); // Prioritize latency
        opts.set("surfaces", "64"); // Number of CUDA surfaces
        opts.set("gpu", "0"); // Use first GPU

        opts
    }

    fn create_filter_graph(
        encoder: &ffmpeg::codec::encoder::Video,
        width: u32,
        height: u32,
    ) -> Result<ffmpeg::filter::Graph> {
        let mut graph = ffmpeg::filter::Graph::new();

        let args = format!(
            "video_size={}x{}:pix_fmt=bgra:time_base=1/1000000",
            width, height
        );

        let mut input = graph.add(&ffmpeg::filter::find("buffer").unwrap(), "in", &args)?;

        // Use hwupload_cuda filter to upload to CUDA
        let mut hwmap = graph.add(
            &ffmpeg::filter::find("hwupload_cuda").unwrap(),
            "hwupload",
            "",
        )?;

        // Scale with CUDA
        let scale_args = format!("w={}:h={}:format=nv12", width, height);
        let mut scale = graph.add(
            &ffmpeg::filter::find("scale_cuda").unwrap(),
            "scale",
            &scale_args,
        )?;

        let mut out = graph.add(&ffmpeg::filter::find("buffersink").unwrap(), "out", "")?;

        unsafe {
            let dev = (*encoder.as_ptr()).hw_device_ctx;
            (*hwmap.as_mut_ptr()).hw_device_ctx = av_buffer_ref(dev);
        }

        input.link(0, &mut hwmap, 0);
        hwmap.link(0, &mut scale, 0);
        scale.link(0, &mut out, 0);

        graph.validate()?;
        log::trace!("Graph\n{}", graph.dump());

        Ok(graph)
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
