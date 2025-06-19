use std::ffi::c_void;

use cust::sys::{CUcontext, CUgraphicsResource, CUresult, CUstream};
use gl::types::{GLenum, GLuint};
use libc::c_uint;

#[repr(C)]
pub struct AVCUDADeviceContext {
    pub cuda_ctx: CUcontext,
    pub stream: CUstream,
    pub internarl: *mut c_void,
}

unsafe extern "C" {
    pub fn cuGraphicsGLRegisterImage(
        resource: *mut CUgraphicsResource,
        image: GLuint,
        target: GLenum,
        flags: c_uint,
    ) -> CUresult;
}
