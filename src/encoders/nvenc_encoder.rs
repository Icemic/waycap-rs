use std::ptr::null_mut;

use drm_fourcc::DrmFourcc;
use ffmpeg_next::{
    self as ffmpeg,
    ffi::{
        av_buffer_create, av_buffer_default_free, av_buffer_ref, av_buffer_unref, av_hwframe_ctx_init, AVDRMFrameDescriptor, AVHWDeviceContext, AVHWFramesContext, AVPixelFormat
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

use super::video::{create_hw_device, create_hw_frame_ctx, VideoEncoder, GOP_SIZE};

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
                log::info!("Got dma buf {:?}", frame);
                let mut drm_frame = ffmpeg::util::frame::Video::new(
                    ffmpeg_next::format::Pixel::DRM_PRIME,
                    encoder.width(),
                    encoder.height(),
                );
                
                unsafe {
                    // Create DRM descriptor that points to the DMA buffer
                    let drm_desc =
                        Box::into_raw(Box::new(std::mem::zeroed::<AVDRMFrameDescriptor>()));

                    (*drm_desc).nb_objects = 1;
                    (*drm_desc).objects[0].fd = fd;
                    (*drm_desc).objects[0].size = frame.size as usize;
                    (*drm_desc).objects[0].format_modifier = frame.modifier;

                    (*drm_desc).nb_layers = 1;
                    (*drm_desc).layers[0].format = DrmFourcc::Argb8888 as u32;
                    (*drm_desc).layers[0].nb_planes = 1;
                    (*drm_desc).layers[0].planes[0].object_index = 0;
                    (*drm_desc).layers[0].planes[0].offset = frame.offset as isize;
                    (*drm_desc).layers[0].planes[0].pitch = frame.stride as isize;

                    // Attach descriptor to frame
                    (*drm_frame.as_mut_ptr()).data[0] = drm_desc as *mut u8;
                    (*drm_frame.as_mut_ptr()).buf[0] = av_buffer_create(
                        drm_desc as *mut u8,
                        std::mem::size_of::<AVDRMFrameDescriptor>(),
                        Some(av_buffer_default_free),
                        null_mut(),
                        0,
                    );

                    (*drm_frame.as_mut_ptr()).hw_frames_ctx =
                        av_buffer_ref((*encoder.as_ptr()).hw_frames_ctx);
                }

                drm_frame.set_pts(Some(frame.timestamp));
                self.filter_graph
                    .as_mut()
                    .unwrap()
                    .get("in")
                    .unwrap()
                    .source()
                    .add(&drm_frame)
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

    fn create_filter_graph(
        encoder: &ffmpeg::codec::encoder::Video,
        width: u32,
        height: u32,
    ) -> Result<ffmpeg::filter::Graph> {
        let mut graph = ffmpeg::filter::Graph::new();

        let args = format!(
            "video_size={}x{}:pix_fmt=drm_prime:time_base=1/1000000",
            width, height
        );

        // TODO: Figure out how to set hw_frames_ctx on input (only see device ctx)
        let mut input = graph.add(&ffmpeg::filter::find("buffer").unwrap(), "in", &args)?;

        let mut hw_upload = graph.add(
            &ffmpeg::filter::find("hwupload_cuda").unwrap(),
            "hwupload_cuda",
            "",
        )?;

        // let scale_args = format!("w={}:h={}:format=nv12:out_range=tv", width, height);
        // let mut scale = graph.add(
        //     &ffmpeg::filter::find("scale_vaapi").unwrap(),
        //     "scale",
        //     &scale_args,
        // )?;

        let mut out = graph.add(&ffmpeg::filter::find("buffersink").unwrap(), "out", "")?;
        unsafe {
            let dev = (*encoder.as_ptr()).hw_device_ctx;

            (*hw_upload.as_mut_ptr()).hw_device_ctx = av_buffer_ref(dev);
        }

        input.link(0, &mut hw_upload, 0);

        unsafe {
            let input_ptr = input.as_mut_ptr();

            let mut av_filter_link = **(*input_ptr).outputs;

        }

        hw_upload.link(0, &mut out, 0);
        // scale.link(0, &mut out, 0);

        graph.validate()?;
        log::info!("NVENC Graph\n{}", graph.dump());

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
