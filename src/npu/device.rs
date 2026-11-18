//! Client for the `intel_vpu` UAPI (`/dev/accel/accel*`).
//!
//! The NPU does not run arbitrary code: the firmware only runs command buffers
//! compiled by Intel's compiler. This opens the device, maps a BO into the NPU
//! virtual address space, and leaves the turbo queue ready. The scrollback
//! index lives in that BO. An optional `RTY1` graph is submitted for real.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

const DRM_IOCTL_VERSION: u64 = 0xc0406400;
const DRM_IOCTL_GEM_CLOSE: u64 = 0x40086409;
const IVPU_GET_PARAM: u64 = 0xc0106440;
const IVPU_BO_CREATE: u64 = 0xc0186442;
const IVPU_BO_INFO: u64 = 0xc0206443;
const IVPU_BO_WAIT: u64 = 0xc0186446;
const IVPU_CMDQ_CREATE: u64 = 0xc00c644b;
const IVPU_CMDQ_DESTROY: u64 = 0x4004644c;
const IVPU_CMDQ_SUBMIT: u64 = 0x4020644d;

const PARAM_DEVICE_ID: u32 = 0;
const PARAM_DEVICE_REVISION: u32 = 1;
const PARAM_PLATFORM_TYPE: u32 = 2;
const PARAM_CORE_CLOCK_RATE: u32 = 3;
const PARAM_NUM_CONTEXTS: u32 = 4;
const PARAM_CONTEXT_BASE_ADDRESS: u32 = 5;
const PARAM_CONTEXT_ID: u32 = 7;
const PARAM_FW_API_VERSION: u32 = 8;
const PARAM_ENGINE_HEARTBEAT: u32 = 9;
const PARAM_TILE_CONFIG: u32 = 11;
const PARAM_SKU: u32 = 12;
const PARAM_CAPABILITIES: u32 = 13;
const PARAM_PREEMPT_BUFFER_SIZE: u32 = 14;

const BO_HIGH_MEM: u32 = 0x1;
const BO_MAPPABLE: u32 = 0x2;
const JOB_PRIORITY_FOCUS: u32 = 3;
const CMDQ_FLAG_TURBO: u32 = 1;
#[repr(C)]
struct DrmVersion {
    major: i32,
    minor: i32,
    patch: i32,
    name_len: usize,
    name: *mut libc::c_char,
    date_len: usize,
    date: *mut libc::c_char,
    desc_len: usize,
    desc: *mut libc::c_char,
}

#[repr(C)]
struct IvpuParam {
    param: u32,
    index: u32,
    value: u64,
}

#[repr(C)]
struct BoCreate {
    size: u64,
    flags: u32,
    handle: u32,
    vpu_addr: u64,
}

#[repr(C)]
struct BoInfo {
    handle: u32,
    flags: u32,
    vpu_addr: u64,
    mmap_offset: u64,
    size: u64,
}

#[repr(C)]
struct GemClose {
    handle: u32,
    pad: u32,
}

#[repr(C)]
struct CmdqCreate {
    cmdq_id: u32,
    priority: u32,
    flags: u32,
}

#[repr(C)]
struct CmdqDestroy {
    cmdq_id: u32,
}

#[repr(C)]
struct CmdqSubmit {
    buffers_ptr: u64,
    buffer_count: u32,
    cmdq_id: u32,
    flags: u32,
    commands_offset: u32,
    preempt_buffer_index: u32,
    reserved: u32,
}

#[repr(C)]
struct BoWait {
    handle: u32,
    flags: u32,
    timeout_ns: i64,
    job_status: u32,
    pad: u32,
}

#[derive(Clone, Debug)]
pub struct NpuInfo {
    pub path: PathBuf,
    pub name: String,
    pub desc: String,
    pub device_id: u32,
    pub revision: u32,
    pub platform: u64,
    pub clock_hz: u64,
    pub max_contexts: u64,
    pub context_base: u64,
    pub context_id: u64,
    pub fw_api: u64,
    pub sku: u64,
    pub tile_config: u64,
    pub preempt_bytes: u64,
    pub cap_metric_streamer: bool,
    pub cap_dma_range: bool,
    pub cap_manage_cmdq: bool,
    pub cap_userptr: bool,
    pub heartbeat: u64,
}

pub struct Device {
    fd: OwnedFd,
    pub info: NpuInfo,
}

impl Device {
    pub fn open() -> io::Result<Self> {
        let mut last = io::Error::new(ErrorKind::NotFound, "nenhuma NPU intel_vpu em /dev/accel");
        for path in accel_paths() {
            match open_path(&path) {
                Ok(device) => return Ok(device),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    pub fn heartbeat(&self) -> io::Result<u64> {
        get_param(self.fd.as_raw_fd(), PARAM_ENGINE_HEARTBEAT, 0)
    }

    pub fn create_buffer(&self, size: u64, mappable: bool) -> io::Result<Buffer> {
        let mut flags = BO_HIGH_MEM;
        if mappable {
            flags |= BO_MAPPABLE;
        }
        let mut create = BoCreate {
            size,
            flags,
            handle: 0,
            vpu_addr: 0,
        };
        ioctl(self.fd.as_raw_fd(), IVPU_BO_CREATE, &mut create)?;
        let mut info = BoInfo {
            handle: create.handle,
            flags: 0,
            vpu_addr: 0,
            mmap_offset: 0,
            size: 0,
        };
        if let Err(err) = ioctl(self.fd.as_raw_fd(), IVPU_BO_INFO, &mut info) {
            let _ = gem_close(self.fd.as_raw_fd(), create.handle);
            return Err(err);
        }
        let mut ptr = std::ptr::null_mut();
        if mappable {
            let mapped = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    info.size as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    self.fd.as_raw_fd(),
                    info.mmap_offset as libc::off_t,
                )
            };
            if mapped == libc::MAP_FAILED {
                let err = io::Error::last_os_error();
                let _ = gem_close(self.fd.as_raw_fd(), create.handle);
                return Err(err);
            }
            ptr = mapped.cast();
            unsafe { std::ptr::write_bytes(ptr, 0, info.size as usize) };
        }
        Ok(Buffer {
            fd: self.fd.as_raw_fd(),
            handle: create.handle,
            size: info.size as usize,
            vpu_addr: info.vpu_addr,
            ptr,
        })
    }

    pub fn create_queue(&self) -> io::Result<CommandQueue> {
        let mut create = CmdqCreate {
            cmdq_id: 0,
            priority: JOB_PRIORITY_FOCUS,
            flags: CMDQ_FLAG_TURBO,
        };
        ioctl(self.fd.as_raw_fd(), IVPU_CMDQ_CREATE, &mut create)?;
        Ok(CommandQueue {
            fd: self.fd.as_raw_fd(),
            id: create.cmdq_id,
        })
    }

    /// Submits a compiled command buffer. `ring` is passed as an argument.
    pub fn submit_graph(
        &self,
        queue: &CommandQueue,
        commands: &[u8],
        ring: &Buffer,
    ) -> io::Result<u32> {
        if commands.is_empty() || commands.len() > 16 * 1024 * 1024 {
            return Err(io::Error::new(ErrorKind::InvalidInput, "command buffer is empty or too large"));
        }
        let preempt_size = self.info.preempt_bytes.max(4096);
        let cmd = self.create_buffer(commands.len().next_multiple_of(4096) as u64, true)?;
        let preempt = self.create_buffer(preempt_size, false)?;
        unsafe {
            std::ptr::copy_nonoverlapping(commands.as_ptr(), cmd.ptr, commands.len());
        }
        let handles = [cmd.handle, ring.handle, preempt.handle];
        let mut submit = CmdqSubmit {
            buffers_ptr: handles.as_ptr() as u64,
            buffer_count: handles.len() as u32,
            cmdq_id: queue.id,
            flags: 0,
            commands_offset: 0,
            preempt_buffer_index: 2,
            reserved: 0,
        };
        ioctl(self.fd.as_raw_fd(), IVPU_CMDQ_SUBMIT, &mut submit)?;
        let mut wait = BoWait {
            handle: cmd.handle,
            flags: 0,
            timeout_ns: monotonic_ns()? + 2_000_000_000,
            job_status: 0,
            pad: 0,
        };
        ioctl(self.fd.as_raw_fd(), IVPU_BO_WAIT, &mut wait)?;
        Ok(wait.job_status)
    }
}

pub struct Buffer {
    fd: RawFd,
    handle: u32,
    pub size: usize,
    pub vpu_addr: u64,
    ptr: *mut u8,
}

impl Buffer {
    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        if self.ptr.is_null() {
            None
        } else {
            Some(unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) })
        }
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { libc::munmap(self.ptr.cast(), self.size) };
        }
        let _ = gem_close(self.fd, self.handle);
    }
}

pub struct CommandQueue {
    fd: RawFd,
    id: u32,
}

impl Drop for CommandQueue {
    fn drop(&mut self) {
        let mut destroy = CmdqDestroy { cmdq_id: self.id };
        let _ = ioctl(self.fd, IVPU_CMDQ_DESTROY, &mut destroy);
    }
}

pub fn load_graph(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 16 || &bytes[..4] != b"RTY1" {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "NPU graph must start with RTY1",
        ));
    }
    let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let end = 16 + len;
    if len == 0 || end > bytes.len() {
        return Err(io::Error::new(ErrorKind::InvalidData, "invalid command buffer size"));
    }
    Ok(bytes[16..end].to_vec())
}

fn open_path(path: &Path) -> io::Result<Device> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let fd = OwnedFd::from(file);
    let raw = fd.as_raw_fd();
    let mut name = [0u8; 64];
    let mut date = [0u8; 64];
    let mut desc = [0u8; 128];
    let mut version = DrmVersion {
        major: 0,
        minor: 0,
        patch: 0,
        name_len: name.len(),
        name: name.as_mut_ptr().cast(),
        date_len: date.len(),
        date: date.as_mut_ptr().cast(),
        desc_len: desc.len(),
        desc: desc.as_mut_ptr().cast(),
    };
    ioctl(raw, DRM_IOCTL_VERSION, &mut version)?;
    let name = cstr(&name);
    if name != "intel_vpu" {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            format!("{path:?} is {name}, not intel_vpu"),
        ));
    }
    let info = NpuInfo {
        path: path.to_path_buf(),
        name,
        desc: cstr(&desc),
        device_id: get_param(raw, PARAM_DEVICE_ID, 0)? as u32,
        revision: get_param(raw, PARAM_DEVICE_REVISION, 0)? as u32,
        platform: get_param(raw, PARAM_PLATFORM_TYPE, 0)?,
        clock_hz: get_param(raw, PARAM_CORE_CLOCK_RATE, 0)?,
        max_contexts: get_param(raw, PARAM_NUM_CONTEXTS, 0)?,
        context_base: get_param(raw, PARAM_CONTEXT_BASE_ADDRESS, 0)?,
        context_id: get_param(raw, PARAM_CONTEXT_ID, 0)?,
        fw_api: get_param(raw, PARAM_FW_API_VERSION, 0)?,
        sku: get_param(raw, PARAM_SKU, 0).unwrap_or(0),
        tile_config: get_param(raw, PARAM_TILE_CONFIG, 0).unwrap_or(0),
        preempt_bytes: get_param(raw, PARAM_PREEMPT_BUFFER_SIZE, 0).unwrap_or(0),
        cap_metric_streamer: get_param(raw, PARAM_CAPABILITIES, 1).unwrap_or(0) != 0,
        cap_dma_range: get_param(raw, PARAM_CAPABILITIES, 2).unwrap_or(0) != 0,
        cap_manage_cmdq: get_param(raw, PARAM_CAPABILITIES, 3).unwrap_or(0) != 0,
        cap_userptr: get_param(raw, PARAM_CAPABILITIES, 4).unwrap_or(0) != 0,
        heartbeat: get_param(raw, PARAM_ENGINE_HEARTBEAT, 0).unwrap_or(0),
    };
    Ok(Device { fd, info })
}

fn accel_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/accel") {
        for entry in entries.flatten() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    paths
}

fn get_param(fd: RawFd, param: u32, index: u32) -> io::Result<u64> {
    let mut args = IvpuParam {
        param,
        index,
        value: 0,
    };
    ioctl(fd, IVPU_GET_PARAM, &mut args)?;
    Ok(args.value)
}

fn gem_close(fd: RawFd, handle: u32) -> io::Result<()> {
    let mut close = GemClose { handle, pad: 0 };
    ioctl(fd, DRM_IOCTL_GEM_CLOSE, &mut close)
}

fn ioctl<T>(fd: RawFd, request: u64, arg: &mut T) -> io::Result<()> {
    let rc = unsafe { libc::ioctl(fd, request as libc::c_ulong, arg) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

fn monotonic_ns() -> io::Result<i64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ts.tv_sec * 1_000_000_000 + ts.tv_nsec as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel_uapi() {
        assert_eq!(std::mem::size_of::<IvpuParam>(), 16);
        assert_eq!(std::mem::size_of::<BoCreate>(), 24);
        assert_eq!(std::mem::size_of::<BoInfo>(), 32);
        assert_eq!(std::mem::size_of::<CmdqCreate>(), 12);
        assert_eq!(std::mem::size_of::<CmdqDestroy>(), 4);
        assert_eq!(std::mem::size_of::<CmdqSubmit>(), 32);
        assert_eq!(std::mem::size_of::<BoWait>(), 24);
        assert_eq!(std::mem::size_of::<GemClose>(), 8);
        assert_eq!(std::mem::size_of::<DrmVersion>(), 64);
    }

    #[test]
    fn opens_maps_and_queues_when_the_npu_is_present() {
        let device = match Device::open() {
            Ok(device) => device,
            Err(err) => {
                eprintln!("NPU absent, test skipped: {err}");
                return;
            }
        };
        assert_ne!(device.info.device_id, 0);
        assert!(device.info.clock_hz > 0);
        let mut buffer = device.create_buffer(4096, true).expect("bo");
        assert!(buffer.vpu_addr > 0);
        let slice = buffer.as_mut_slice().expect("mmap");
        slice[0] = 0x5a;
        assert_eq!(slice[0], 0x5a);
        let queue = device.create_queue().expect("cmdq");
        drop(queue);
    }
}
