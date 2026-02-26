// shared_frame_shm.rs (trixie — compositor, writer only)
// The compositor creates and writes the shm region.
// trixterm opens it read-only via its own shared_frame_shm.rs.

use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const MAX_PIXELS: usize = 3840 * 2160 * 4;
pub const SHM_SIZE: usize = std::mem::size_of::<ShmHeader>() + MAX_PIXELS;

pub fn shm_name(app_id: &str) -> String {
    format!("/trixie-embed-{app_id}")
}

#[repr(C)]
pub struct ShmHeader {
    pub serial: AtomicU64,
    pub width: AtomicU32,
    pub height: AtomicU32,
}

// ── Writer ────────────────────────────────────────────────────────────────────

pub struct ShmWriter {
    ptr: *mut u8,
    fd: RawFd,
    app_id: String,
}

unsafe impl Send for ShmWriter {}

impl ShmWriter {
    pub fn create(app_id: &str) -> Result<Self, String> {
        let name = shm_name(app_id);
        let c_name = CString::new(name.as_str()).map_err(|e| e.to_string())?;

        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_CREAT | libc::O_RDWR | libc::O_TRUNC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "shm_open({name}): {}",
                std::io::Error::last_os_error()
            ));
        }

        if unsafe { libc::ftruncate(fd, SHM_SIZE as libc::off_t) } < 0 {
            unsafe { libc::close(fd) };
            return Err(format!("ftruncate: {}", std::io::Error::last_os_error()));
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                SHM_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return Err(format!("mmap: {}", std::io::Error::last_os_error()));
        }

        unsafe { std::ptr::write_bytes(ptr as *mut u8, 0, std::mem::size_of::<ShmHeader>()) };

        tracing::debug!("ShmWriter: created '{}'", name);
        Ok(Self {
            ptr: ptr as *mut u8,
            fd,
            app_id: app_id.to_owned(),
        })
    }

    fn header(&self) -> &ShmHeader {
        unsafe { &*(self.ptr as *const ShmHeader) }
    }
    fn pixel_ptr(&self) -> *mut u8 {
        unsafe { self.ptr.add(std::mem::size_of::<ShmHeader>()) }
    }

    pub fn write_frame(&self, pixels: &[u8], width: u32, height: u32) {
        let hdr = self.header();
        let byte_count = ((width * height * 4) as usize)
            .min(MAX_PIXELS)
            .min(pixels.len());

        let prev = hdr.serial.load(Ordering::Relaxed);
        hdr.serial.store(prev | 1, Ordering::Release);
        hdr.width.store(width, Ordering::Relaxed);
        hdr.height.store(height, Ordering::Relaxed);
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.pixel_ptr(), byte_count) };
        hdr.serial.store(prev + 2, Ordering::Release);
    }
}

impl Drop for ShmWriter {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, SHM_SIZE);
            libc::close(self.fd);
            if let Ok(c) = CString::new(shm_name(&self.app_id)) {
                libc::shm_unlink(c.as_ptr());
            }
        }
    }
}
