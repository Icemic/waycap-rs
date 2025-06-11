use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};

// Type definitions
pub type CUcontext = *mut c_void;
pub type CUstream = *mut c_void;
pub type CUgraphicsResource = *mut c_void;
pub type CUarray = *mut c_void;
pub type CUresult = u32;
pub type CUdeviceptr = u64;
pub type CUdevice = c_int;

// Constants for context creation
pub const CU_CTX_SCHED_AUTO: u32 = 0x00;
pub const CU_CTX_SCHED_SPIN: u32 = 0x01;
pub const CU_CTX_SCHED_YIELD: u32 = 0x02;
pub const CU_CTX_SCHED_BLOCKING_SYNC: u32 = 0x04;

// Graphics resource flags
pub const CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE: u32 = 0x00;
pub const CU_GRAPHICS_REGISTER_FLAGS_NONE: u32 = 0x00;
pub const CU_GRAPHICS_REGISTER_FLAGS_READ_NONE: u32 = 0x00;
pub const CU_GRAPHICS_REGISTER_FLAGS_WRITE_DISCARD: u32 = 0x01;
pub const CU_GRAPHICS_REGISTER_FLAGS_SURFACE_LDST: u32 = 0x02;
pub const CU_GRAPHICS_REGISTER_FLAGS_TEXTURE_GATHER: u32 = 0x04;

// Memory type enumeration
#[repr(C)]
pub enum CUmemorytype {
    CU_MEMORYTYPE_HOST = 0x01,
    CU_MEMORYTYPE_DEVICE = 0x02,
    CU_MEMORYTYPE_ARRAY = 0x03,
    CU_MEMORYTYPE_UNIFIED = 0x04,
}

// CUDA 2D memory copy structure
#[repr(C)]
pub struct CUDA_MEMCPY2D {
    pub srcXInBytes: usize,
    pub srcY: usize,
    pub srcMemoryType: u32,
    pub srcHost: *const c_void,
    pub srcDevice: CUdeviceptr,
    pub srcArray: CUarray,
    pub srcPitch: usize,
    pub dstXInBytes: usize,
    pub dstY: usize,
    pub dstMemoryType: u32,
    pub dstHost: *mut c_void,
    pub dstDevice: CUdeviceptr,
    pub dstArray: CUarray,
    pub dstPitch: usize,
    pub WidthInBytes: usize,
    pub Height: usize,
}

// Array format enumeration
#[repr(C)]
pub enum CUarray_format {
    CU_AD_FORMAT_UNSIGNED_INT8 = 0x01,
    CU_AD_FORMAT_UNSIGNED_INT16 = 0x02,
    CU_AD_FORMAT_UNSIGNED_INT32 = 0x03,
    CU_AD_FORMAT_SIGNED_INT8 = 0x08,
    CU_AD_FORMAT_SIGNED_INT16 = 0x09,
    CU_AD_FORMAT_SIGNED_INT32 = 0x0a,
    CU_AD_FORMAT_HALF = 0x10,
    CU_AD_FORMAT_FLOAT = 0x20,
}

// CUDA array descriptor structure
#[repr(C)]
pub struct CUDA_ARRAY_DESCRIPTOR {
    pub Format: CUarray_format,
    pub Height: usize,
    pub Width: usize,
    pub NumChannels: u32,
}

// FFmpeg CUDA device context structure
#[repr(C)]
pub struct AVCUDADeviceContext {
    pub cuda_ctx: CUcontext,
    pub stream: CUstream,
    pub internal: *mut std::ffi::c_void, // AVCUDADeviceContextInternal*
}

// External function declarations
extern "C" {
    // CUDA Driver API initialization
    /// Initialize CUDA driver API
    pub fn cuInit(flags: c_uint) -> CUresult;

    // Device management
    /// Get number of CUDA devices
    pub fn cuDeviceGetCount(count: *mut c_int) -> CUresult;

    /// Get CUDA device handle
    pub fn cuDeviceGet(device: *mut CUdevice, ordinal: c_int) -> CUresult;

    /// Get device properties
    pub fn cuDeviceGetName(name: *mut u8, len: c_int, dev: CUdevice) -> CUresult;

    /// Get device attribute
    pub fn cuDeviceGetAttribute(pi: *mut c_int, attrib: c_uint, dev: CUdevice) -> CUresult;

    // Context management
    /// Create CUDA context with OpenGL interop (modern way)
    pub fn cuGLCtxCreate_v2(pCtx: *mut CUcontext, flags: c_uint, device: CUdevice) -> CUresult;

    /// Create regular CUDA context
    pub fn cuCtxCreate_v2(pCtx: *mut CUcontext, flags: c_uint, device: CUdevice) -> CUresult;

    /// Destroy CUDA context
    pub fn cuCtxDestroy_v2(ctx: CUcontext) -> CUresult;

    /// Set current CUDA context
    pub fn cuCtxSetCurrent(ctx: CUcontext) -> CUresult;

    /// Get current CUDA context
    pub fn cuCtxGetCurrent(pCtx: *mut CUcontext) -> CUresult;

    /// Push context onto current thread's context stack
    pub fn cuCtxPushCurrent_v2(ctx: CUcontext) -> CUresult;

    /// Pop context from current thread's context stack
    pub fn cuCtxPopCurrent_v2(pCtx: *mut CUcontext) -> CUresult;

    /// Get device associated with current context
    pub fn cuCtxGetDevice(device: *mut CUdevice) -> CUresult;

    /// Synchronize context
    pub fn cuCtxSynchronize() -> CUresult;

    // OpenGL interop functions
    /// Register OpenGL image with CUDA
    pub fn cuGraphicsGLRegisterImage(
        resource: *mut CUgraphicsResource,
        image: u32,
        target: u32,
        flags: u32,
    ) -> CUresult;

    /// Set map flags for graphics resource
    pub fn cuGraphicsResourceSetMapFlags(resource: CUgraphicsResource, flags: u32) -> CUresult;

    /// Map graphics resources for CUDA access
    pub fn cuGraphicsMapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> CUresult;

    /// Unmap graphics resources
    pub fn cuGraphicsUnmapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> CUresult;

    /// Get mapped array from graphics resource
    pub fn cuGraphicsSubResourceGetMappedArray(
        array: *mut CUarray,
        resource: CUgraphicsResource,
        array_index: u32,
        mip_level: u32,
    ) -> CUresult;

    /// Unregister graphics resource
    pub fn cuGraphicsUnregisterResource(resource: CUgraphicsResource) -> CUresult;

    // Memory operations
    /// 2D memory copy
    pub fn cuMemcpy2D(pCopy: *const CUDA_MEMCPY2D) -> CUresult;

    /// Get array descriptor
    pub fn cuArrayGetDescriptor(
        pArrayDescriptor: *mut CUDA_ARRAY_DESCRIPTOR,
        hArray: CUarray,
    ) -> CUresult;
}
