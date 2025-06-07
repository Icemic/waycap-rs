use std::ffi::c_void;
use std::os::raw::c_uint;

pub type CUcontext = *mut c_void;
pub type CUstream = *mut c_void;
pub type CUgraphicsResource = *mut c_void;
pub type CUarray = *mut c_void;

#[repr(C)]
pub struct CUDA_MEMCPY2D {
    pub srcXInBytes: usize,
    pub srcY: usize,
    pub srcMemoryType: i32,
    pub srcHost: *const c_void,
    pub srcDevice: *const c_void,
    pub srcArray: CUarray,
    pub srcPitch: usize,

    pub dstXInBytes: usize,
    pub dstY: usize,
    pub dstMemoryType: i32,
    pub dstHost: *mut c_void,
    pub dstDevice: *mut c_void,
    pub dstArray: CUarray,
    pub dstPitch: usize,

    pub WidthInBytes: usize,
    pub Height: usize,
}

extern "C" {
    pub fn cuCtxSetCurrent(ctx: CUcontext) -> i32;

    pub fn cuGraphicsGLRegisterImage(
        resource: *mut CUgraphicsResource,
        image: u32,
        target: u32,
        flags: u32,
    ) -> i32;

    pub fn cuGraphicsMapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> i32;

    pub fn cuGraphicsSubResourceGetMappedArray(
        array: *mut CUarray,
        resource: CUgraphicsResource,
        array_index: u32,
        mip_level: u32,
    ) -> i32;

    pub fn cuGraphicsUnmapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> i32;

    pub fn cuGraphicsUnregisterResource(resource: CUgraphicsResource) -> i32;

    pub fn cuMemcpy2D(pCopy: *const CUDA_MEMCPY2D) -> i32;

    // TODO: Figure out how to get egl and cuda to share the context
    // Looks like I need to interop with GL to get dma buffer support for nvenc :(
    pub fn cuGLCtxCreate(pCtx: *const CUcontext, flags: u32, device: i32) -> i32;
}

#[repr(C)]
pub struct AVCUDADeviceContext {
    pub cuda_ctx: CUcontext,
    pub stream: CUstream,
    pub internal: *mut std::ffi::c_void, // AVCUDADeviceContextInternal*
}
