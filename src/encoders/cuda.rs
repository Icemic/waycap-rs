use std::ffi::c_void;
use std::os::raw::c_uint;

pub type CUcontext = *mut c_void;
pub type CUstream = *mut c_void;
pub type CUgraphicsResource = *mut c_void;
pub type CUarray = *mut c_void;
pub type CUresult = u32;
pub type CUdeviceptr = u64;
pub const CU_GRAPHICS_MAP_RESOURCE_FLAGS_NONE: u32 = 0x00;

#[repr(C)]
pub enum CUmemorytype {
    CU_MEMORYTYPE_HOST = 0x01,
    CU_MEMORYTYPE_DEVICE = 0x02,
    CU_MEMORYTYPE_ARRAY = 0x03,
    CU_MEMORYTYPE_UNIFIED = 0x04,
}

#[repr(C)]
pub struct CUDA_MEMCPY2D {
    pub srcXInBytes: usize, // size_t
    pub srcY: usize,        // size_t
    pub srcMemoryType: u32, // CUmemorytype
    pub srcHost: *const c_void,
    pub srcDevice: CUdeviceptr, // u64
    pub srcArray: CUarray,      // *mut c_void
    pub srcPitch: usize,        // size_t

    pub dstXInBytes: usize, // size_t
    pub dstY: usize,        // size_t
    pub dstMemoryType: u32, // CUmemorytype
    pub dstHost: *mut c_void,
    pub dstDevice: CUdeviceptr, // u64
    pub dstArray: CUarray,      // *mut c_void
    pub dstPitch: usize,        // size_t

    pub WidthInBytes: usize, // size_t
    pub Height: usize,       // size_t
}

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

#[repr(C)]
pub struct CUDA_ARRAY_DESCRIPTOR {
    pub Format: CUarray_format,
    pub Height: usize,
    pub Width: usize,
    pub NumChannels: u32,
}

extern "C" {
    pub fn cuCtxSetCurrent(ctx: CUcontext) -> CUresult;

    pub fn cuGraphicsGLRegisterImage(
        resource: *mut CUgraphicsResource,
        image: u32,
        target: u32,
        flags: u32,
    ) -> CUresult;

    pub fn cuGraphicsMapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> CUresult;

    pub fn cuGraphicsSubResourceGetMappedArray(
        array: *mut CUarray,
        resource: CUgraphicsResource,
        array_index: u32,
        mip_level: u32,
    ) -> CUresult;

    pub fn cuGraphicsUnmapResources(
        count: c_uint,
        resources: *const CUgraphicsResource,
        stream: CUstream,
    ) -> CUresult;

    pub fn cuGraphicsUnregisterResource(resource: CUgraphicsResource) -> CUresult;

    pub fn cuMemcpy2D(pCopy: *const CUDA_MEMCPY2D) -> CUresult;

    pub fn cuCtxGetCurrent(pCtx: *const CUcontext) -> CUresult;

    pub fn cuArrayGetDescriptor(
        pArrayDescriptor: *mut CUDA_ARRAY_DESCRIPTOR,
        hArray: *mut c_void,
    ) -> CUresult;

    pub fn cuGraphicsResourceSetMapFlags(resource: CUgraphicsResource, flags: u32) -> CUresult;
}

#[repr(C)]
pub struct AVCUDADeviceContext {
    pub cuda_ctx: CUcontext,
    pub stream: CUstream,
    pub internal: *mut std::ffi::c_void, // AVCUDADeviceContextInternal*
}
