use std::ffi::c_void;

use khronos_egl::{self as egl, ClientBuffer, Dynamic, Instance};

use crate::types::video_frame::DmaBufPlane;

type PFNGLEGLIMAGETARGETTEXTURE2DOESPROC =
    unsafe extern "C" fn(target: gl::types::GLenum, image: *const c_void);

unsafe impl Sync for EglContext {}
unsafe impl Send for EglContext {}

pub struct EglContext {
    egl_instance: Instance<Dynamic<libloading::Library, egl::EGL1_5>>,
    display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
    _config: egl::Config,
    dmabuf_supported: bool,
    dmabuf_modifiers_supported: bool,

    // Keep Wayland display alive
    _wayland_display: wayland_client::Display,
}

impl EglContext {
    pub fn new(width: i32, height: i32) -> Result<Self, egl::Error> {
        let lib =
            unsafe { libloading::Library::new("libEGL.so.1") }.expect("unable to find libEGL.so.1");
        let egl_instance = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required_from(lib) }
            .expect("unable to load libEGL.so.1");

        egl_instance.bind_api(egl::OPENGL_ES_API)?;

        let wayland_display = wayland_client::Display::connect_to_env().unwrap();
        let display =
            unsafe { egl_instance.get_display(wayland_display.c_ptr() as *mut std::ffi::c_void) }
                .unwrap();
        egl_instance.initialize(display)?;

        let attributes = [
            egl::BUFFER_SIZE,
            24,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
            egl::NONE,
            egl::NONE,
        ];
        let config = egl_instance
            .choose_first_config(display, &attributes)?
            .expect("unable to find an appropriate ELG configuration");

        egl_instance.bind_api(egl::OPENGL_ES_API)?;

        let context_attributes = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];

        let context = egl_instance.create_context(display, config, None, &context_attributes)?;

        let surface_attributes = [egl::WIDTH, width, egl::HEIGHT, height, egl::NONE];

        let surface = egl_instance.create_pbuffer_surface(display, config, &surface_attributes)?;
        egl_instance.make_current(display, Some(surface), Some(surface), Some(context))?;

        gl::load_with(|symbol| egl_instance.get_proc_address(&symbol).unwrap() as *const _);

        let (dmabuf_supported, dmabuf_modifiers_supported) =
            Self::check_dmabuf_support(&egl_instance, display).unwrap();

        log::info!("Created egl context");

        Ok(Self {
            egl_instance,
            display,
            _config: config,
            context,
            surface,
            dmabuf_supported,
            dmabuf_modifiers_supported,

            _wayland_display: wayland_display,
        })
    }

    fn check_dmabuf_support(
        egl_instance: &Instance<Dynamic<libloading::Library, egl::EGL1_5>>,
        display: egl::Display,
    ) -> Result<(bool, bool), Box<dyn std::error::Error>> {
        let extensions = egl_instance.query_string(Some(display), egl::EXTENSIONS)?;
        let ext_str = extensions.to_string_lossy();

        let dmabuf_import = ext_str.contains("EGL_EXT_image_dma_buf_import");
        let dmabuf_modifiers = ext_str.contains("EGL_EXT_image_dma_buf_import_modifiers");

        if !dmabuf_import {
            return Err("EGL_EXT_image_dma_buf_import not supported".into());
        }

        Ok((dmabuf_import, dmabuf_modifiers))
    }

    pub fn create_image_from_dmabuf(
        &self,
        planes: &[DmaBufPlane],
        format: u32,
        width: u32,
        height: u32,
        modifier: u64,
    ) -> Result<egl::Image, Box<dyn std::error::Error>> {
        if !self.dmabuf_supported {
            return Err("DMA-BUF import not supported".into());
        }

        let mut attributes = vec![
            // EGL_LINUX_DRM_FOURCC_EXT
            0x3271,
            format as usize,
            egl::WIDTH as usize,
            width as usize,
            egl::HEIGHT as usize,
            height as usize,
        ];

        for (i, plane) in planes.iter().enumerate() {
            let plane_attrs = match i {
                0 => vec![
                    // EGL_DMA_BUF_PLANE0_FD_EXT
                    0x3272,
                    plane.fd as usize,
                    // EGL_DMA_BUF_PLANE0_OFFSET_EXT
                    0x3273,
                    plane.offset as usize,
                    // EGL_DMA_BUF_PLANE0_PITCH_EXT
                    0x3274,
                    plane.stride as usize,
                ],
                1 => vec![
                    // EGL_DMA_BUF_PLANE1_FD_EXT
                    0x3275,
                    plane.fd as usize,
                    // EGL_DMA_BUF_PLANE1_OFFSET_EXT
                    0x3276,
                    plane.offset as usize,
                    // EGL_DMA_BUF_PLANE1_PITCH_EXT
                    0x3277,
                    plane.stride as usize,
                ],
                2 => vec![
                    // EGL_DMA_BUF_PLANE2_FD_EXT
                    0x3278,
                    plane.fd as usize,
                    // EGL_DMA_BUF_PLANE2_OFFSET_EXT
                    0x3279,
                    plane.offset as usize,
                    // EGL_DMA_BUF_PLANE2_PITCH_EXT
                    0x327A,
                    plane.stride as usize,
                ],
                _ => break,
            };

            attributes.extend(plane_attrs);

            // Add modifiers if supported
            if self.dmabuf_modifiers_supported {
                let modifier_attrs = match i {
                    0 => vec![
                        // EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT
                        0x3443,
                        (modifier & 0xFFFFFFFF) as usize,
                        // EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT
                        0x3444,
                        (modifier >> 32) as usize,
                    ],
                    1 => vec![
                        // EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT
                        0x3445,
                        (modifier & 0xFFFFFFFF) as usize,
                        // EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT
                        0x3446,
                        (modifier >> 32) as usize,
                    ],
                    2 => vec![
                        // EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT
                        0x3447,
                        (modifier & 0xFFFFFFFF) as usize,
                        // EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT
                        0x3448,
                        (modifier >> 32) as usize,
                    ],
                    _ => break,
                };
                attributes.extend(modifier_attrs);
            }
        }

        attributes.push(egl::NONE as usize);

        // Create EGL image
        let image = self
            .egl_instance
            .create_image(
                self.display,
                unsafe { egl::Context::from_ptr(egl::NO_CONTEXT) },
                // EGL_LINUX_DMA_BUF_EXT
                0x3270,
                unsafe { ClientBuffer::from_ptr(std::ptr::null_mut()) },
                &attributes,
            )
            .map_err(|e| format!("Failed to create EGL image from DMA-BUF: {:?}", e))?;

        Ok(image)
    }

    pub fn extract_pixels_from_egl_image(
        &self,
        image: &egl::Image,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        unsafe {
            // Create framebuffer and texture
            let mut fbo = 0;
            let mut texture = 0;

            gl::GenFramebuffers(1, &mut fbo);
            gl::GenTextures(1, &mut texture);

            // Bind texture and import EGL image
            gl::BindTexture(gl::TEXTURE_2D, texture);
            let egl_texture_2d = {
                let proc_name = "glEGLImageTargetTexture2DOES";
                let proc_addr = self.egl_instance.get_proc_address(proc_name);

                if proc_addr.is_none() {
                    None
                } else {
                    Some(std::mem::transmute::<_, PFNGLEGLIMAGETARGETTEXTURE2DOESPROC>(proc_addr))
                }
            };

            egl_texture_2d.unwrap()(gl::TEXTURE_2D, image.as_ptr());

            // Check for GL errors after importing
            let gl_error = gl::GetError();
            if gl_error != gl::NO_ERROR {
                gl::DeleteFramebuffers(1, &fbo);
                gl::DeleteTextures(1, &texture);
                return Err(
                    format!("Failed to import EGL image to texture: 0x{:x}", gl_error).into(),
                );
            }

            // Set up framebuffer
            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                texture,
                0,
            );

            // Check framebuffer status
            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                gl::DeleteFramebuffers(1, &fbo);
                gl::DeleteTextures(1, &texture);
                gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                return Err(format!("Framebuffer not complete: 0x{:x}", status).into());
            }

            // Set viewport
            gl::Viewport(0, 0, width as i32, height as i32);

            // Allocate pixel buffer (RGBA format)
            let pixel_count = (width * height * 4) as usize;
            let mut pixels = vec![0u8; pixel_count];

            // Read pixels from framebuffer
            gl::ReadPixels(
                0,
                0,                 // x, y offset
                width as i32,      // width
                height as i32,     // height
                gl::RGBA,          // format
                gl::UNSIGNED_BYTE, // type
                pixels.as_mut_ptr() as *mut std::ffi::c_void,
            );

            // Check for read errors
            let gl_error = gl::GetError();
            if gl_error != gl::NO_ERROR {
                gl::DeleteFramebuffers(1, &fbo);
                gl::DeleteTextures(1, &texture);
                gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                return Err(format!("Failed to read pixels: 0x{:x}", gl_error).into());
            }

            // Cleanup
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::DeleteFramebuffers(1, &fbo);
            gl::DeleteTextures(1, &texture);

            Ok(pixels)
        }
    }

    pub fn save_pixels_as_png(
        pixels: &[u8],
        width: u32,
        height: u32,
        filename: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use image::{ImageBuffer, RgbaImage};

        let img: RgbaImage = ImageBuffer::from_raw(width, height, pixels.to_vec())
            .ok_or("Failed to create image buffer")?;

        img.save(filename)?;
        Ok(())
    }

    pub fn bind_image_to_texture(
        &self,
        image: egl::Image,
    ) -> Result<u32, Box<dyn std::error::Error>> {
        let mut texture_id = 0;
        unsafe {
            gl::GenTextures(1, &mut texture_id);

            // Try GL_TEXTURE_2D first
            gl::BindTexture(gl::TEXTURE_2D, texture_id);

            // Load the extension function

            let egl_texture_2d = {
                let proc_name = "glEGLImageTargetTexture2DOES";
                let proc_addr = self.egl_instance.get_proc_address(proc_name);

                if proc_addr.is_none() {
                    None
                } else {
                    Some(std::mem::transmute::<_, PFNGLEGLIMAGETARGETTEXTURE2DOESPROC>(proc_addr))
                }
            };

            egl_texture_2d.unwrap()(gl::TEXTURE_2D, image.as_ptr());

            let mut width = 0;
            let mut height = 0;
            let mut internal_format = 0;
            gl::GetTexLevelParameteriv(gl::TEXTURE_2D, 0, gl::TEXTURE_WIDTH, &mut width);
            gl::GetTexLevelParameteriv(gl::TEXTURE_2D, 0, gl::TEXTURE_HEIGHT, &mut height);
            gl::GetTexLevelParameteriv(
                gl::TEXTURE_2D,
                0,
                gl::TEXTURE_INTERNAL_FORMAT,
                &mut internal_format,
            );

            log::info!(
                "EGL image created texture with format: 0x{:x}",
                internal_format
            );

            if internal_format == gl::RGBA as i32 {
                log::info!("Converting unsized RGBA to RGBA8 for CUDA compatibility");

                // Create a new texture with proper format
                let mut cuda_compatible_texture = 0;
                gl::GenTextures(1, &mut cuda_compatible_texture);
                gl::BindTexture(gl::TEXTURE_2D, cuda_compatible_texture);

                // Allocate with RGBA8 format
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA8 as i32, // CUDA-compatible format
                    width as i32,
                    height as i32,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    std::ptr::null(),
                );

                // Copy from EGL texture to CUDA-compatible texture using FBO
                let mut fbo = 0;
                gl::GenFramebuffers(1, &mut fbo);
                gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);

                // Attach EGL texture as source
                gl::FramebufferTexture2D(
                    gl::FRAMEBUFFER,
                    gl::COLOR_ATTACHMENT0,
                    gl::TEXTURE_2D,
                    texture_id, // EGL texture
                    0,
                );

                // Check framebuffer status
                let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
                if status != gl::FRAMEBUFFER_COMPLETE {
                    gl::DeleteFramebuffers(1, &fbo);
                    gl::DeleteTextures(1, &cuda_compatible_texture);
                    gl::DeleteTextures(1, &texture_id);
                    return Err(format!("Framebuffer not complete: 0x{:x}", status).into());
                }

                // Copy to CUDA-compatible texture
                gl::BindTexture(gl::TEXTURE_2D, cuda_compatible_texture);
                gl::CopyTexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA8, // CUDA-compatible format
                    0,
                    0, // x, y offset in framebuffer
                    width as i32,
                    height as i32,
                    0, // border
                );

                // Cleanup
                gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                gl::DeleteFramebuffers(1, &fbo);
                gl::DeleteTextures(1, &texture_id); // Delete EGL texture

                // Return the CUDA-compatible texture
                gl::BindTexture(gl::TEXTURE_2D, 0);

                log::info!(
                    "✓ Created CUDA-compatible texture: ID {}",
                    cuda_compatible_texture
                );
                return Ok(cuda_compatible_texture);
            }

            if gl::GetError() == gl::NO_ERROR {
                let mut width = 0;
                gl::GetTexLevelParameteriv(gl::TEXTURE_2D, 0, gl::TEXTURE_WIDTH, &mut width);
                if width == 0 {
                    log::warn!("Texture has zero width after bind");
                }

                gl::BindTexture(gl::TEXTURE_2D, 0);

                log::info!("✓ Bound EGL image to GL_TEXTURE_2D: {}", texture_id);
                return Ok(texture_id);
            }

            // Fallback to GL_TEXTURE_EXTERNAL_OES
            // TEXTURE_EXTERNAL_OES
            gl::BindTexture(0x8D65, texture_id);
            egl_texture_2d.unwrap()(0x8D65, image.as_ptr());
            gl::BindTexture(0x8D65, 0);

            if gl::GetError() != gl::NO_ERROR {
                gl::DeleteTextures(1, &texture_id);
                return Err("Failed to bind EGL image to texture".into());
            }

            log::info!(
                "✓ Bound EGL image to GL_TEXTURE_EXTERNAL_OES: {}",
                texture_id
            );
        }

        Ok(texture_id)
    }

    pub fn destroy_image(&self, image: egl::Image) -> Result<(), Box<dyn std::error::Error>> {
        self.egl_instance
            .destroy_image(self.display, image)
            .map_err(|e| format!("Failed to destroy EGL image: {:?}", e).into())
    }

    pub fn delete_texture(&self, texture_id: u32) {
        unsafe {
            gl::DeleteTextures(1, &texture_id);
        }
    }

    pub fn make_current(&self) -> Result<(), egl::Error> {
        self.egl_instance.make_current(
            self.display,
            Some(self.surface),
            Some(self.surface),
            Some(self.context),
        )?;
        Ok(())
    }

    pub fn release_current(&self) -> Result<(), egl::Error> {
        self.egl_instance
            .make_current(self.display, None, None, None)?;
        Ok(())
    }

    pub fn get_egl_instance(&self) -> &Instance<Dynamic<libloading::Library, egl::EGL1_5>> {
        &self.egl_instance
    }

    pub fn get_display(&self) -> egl::Display {
        self.display
    }

    pub fn test_opengl(&self) -> Result<(), Box<dyn std::error::Error>> {
        let version = unsafe {
            let data = gl::GetString(gl::VERSION) as *const i8;
            if data.is_null() {
                "Unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(data)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        log::info!("OpenGL Version: {}", version);

        let renderer = unsafe {
            let data = gl::GetString(gl::RENDERER) as *const i8;
            if data.is_null() {
                "Unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(data)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        log::info!("OpenGL Renderer: {}", renderer);

        // Test texture creation
        let mut texture_id = 0;
        unsafe {
            gl::GenTextures(1, &mut texture_id);
            if texture_id != 0 {
                println!("✓ Texture created: ID {}", texture_id);
                gl::DeleteTextures(1, &texture_id);
            } else {
                return Err("Failed to create texture".into());
            }
        }

        Ok(())
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        let _ = self
            .egl_instance
            .make_current(self.display, None, None, None);
        let _ = self
            .egl_instance
            .destroy_surface(self.display, self.surface);
        let _ = self
            .egl_instance
            .destroy_context(self.display, self.context);
        let _ = self.egl_instance.terminate(self.display);
    }
}
