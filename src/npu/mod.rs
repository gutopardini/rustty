//! Scrollback index resident in the NPU virtual address space.
//!
//! Each line that leaves the screen becomes a 64-byte record (AVX2 hash plus
//! a preview). The ring lives in a GEM buffer mapped on the NPU. Without a
//! compiled graph the firmware is not asked to compute. The data is already
//! in place for an `RTY1` command buffer.

mod device;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

pub use device::{load_graph, Device, NpuInfo};

use crate::cell::Cell;
use crate::simd::hash_cells;

const RING_BYTES: u64 = 1024 * 1024;
const HEADER_BYTES: usize = 64;
const ENTRY_BYTES: usize = 64;

#[repr(C)]
struct RingHeader {
    magic: u32,
    version: u32,
    capacity: u32,
    write_index: u32,
    count: u32,
    heartbeat: u32,
    device_id: u32,
    clock_mhz: u32,
    indexed: u64,
    dropped: u64,
    vpu_addr: u64,
    reserved: u64,
}

pub struct Status {
    pub present: AtomicBool,
    pub device_id: AtomicU64,
    pub clock_mhz: AtomicU64,
    pub indexed: AtomicU64,
    pub dropped: AtomicU64,
    pub heartbeat: AtomicU64,
    pub vpu_addr: AtomicU64,
}

impl Status {
    fn new() -> Self {
        Self {
            present: AtomicBool::new(false),
            device_id: AtomicU64::new(0),
            clock_mhz: AtomicU64::new(0),
            indexed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            heartbeat: AtomicU64::new(0),
            vpu_addr: AtomicU64::new(0),
        }
    }
}

enum Job {
    Records(Vec<[u8; 64]>),
    Stop,
}

pub struct Scheduler {
    tx: SyncSender<Job>,
    join: Option<JoinHandle<()>>,
    pub status: Arc<Status>,
}

impl Scheduler {
    pub fn start() -> Self {
        let (tx, rx) = sync_channel(256);
        let status = Arc::new(Status::new());
        let status_thread = Arc::clone(&status);
        let join = std::thread::Builder::new()
            .name("rustty-npu".into())
            .spawn(move || worker(rx, status_thread))
            .ok();
        Self {
            tx,
            join,
            status,
        }
    }

    pub fn index_records(&self, records: Vec<[u8; 64]>) {
        self.indexer().index_records(records);
    }

    pub fn indexer(&self) -> Indexer {
        Indexer {
            tx: self.tx.clone(),
            status: Arc::clone(&self.status),
        }
    }
}

#[derive(Clone)]
pub struct Indexer {
    tx: SyncSender<Job>,
    status: Arc<Status>,
}

impl Indexer {
    pub fn index_records(&self, records: Vec<[u8; 64]>) {
        if records.is_empty() {
            return;
        }
        let dropped = records.len() as u64;
        match self.tx.try_send(Job::Records(records)) {
            Ok(()) => {}
            Err(TrySendError::Full(Job::Records(_))) => {
                self.status.dropped.fetch_add(dropped, Ordering::Relaxed);
            }
            Err(_) => {}
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let _ = self.tx.try_send(Job::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn worker(rx: Receiver<Job>, status: Arc<Status>) {
    match Device::open() {
        Ok(device) => {
            tracing::info!(
                path = %device.info.path.display(),
                device = format_args!("{:#06x}", device.info.device_id),
                clock_mhz = device.info.clock_hz / 1_000_000,
                fw = format_args!("{:#x}", device.info.fw_api),
                context = device.info.context_id,
                "Intel NPU open"
            );
            status.present.store(true, Ordering::Relaxed);
            status.device_id.store(u64::from(device.info.device_id), Ordering::Relaxed);
            status.clock_mhz.store(device.info.clock_hz / 1_000_000, Ordering::Relaxed);
            if let Err(err) = device_loop(device, rx, &status) {
                tracing::warn!(%err, "NPU queue dropped, index stays on the CPU");
                status.present.store(false, Ordering::Relaxed);
            }
        }
        Err(err) => {
            tracing::info!(%err, "no NPU, scrollback index stays on the CPU");
            host_loop(rx, &status);
        }
    }
}

fn device_loop(device: Device, rx: Receiver<Job>, status: &Status) -> std::io::Result<()> {
    let mut ring = device.create_buffer(RING_BYTES, true)?;
    let queue = device.create_queue()?;
    let vpu_addr = ring.vpu_addr;
    status.vpu_addr.store(vpu_addr, Ordering::Relaxed);
    let capacity = ((ring.size.saturating_sub(4096)) / ENTRY_BYTES) as u32;
    {
        let slice = ring.as_mut_slice().expect("mapped ring");
        write_header(
            slice,
            &RingHeader {
                magic: u32::from_le_bytes(*b"RTYN"),
                version: 1,
                capacity,
                write_index: 0,
                count: 0,
                heartbeat: device.heartbeat().unwrap_or(0) as u32,
                device_id: device.info.device_id,
                clock_mhz: (device.info.clock_hz / 1_000_000) as u32,
                indexed: 0,
                dropped: 0,
                vpu_addr,
                reserved: 0,
            },
        );
    }
    maybe_submit_graph(&device, &queue, &ring);
    let mut write_index = 0u32;
    let mut indexed = 0u64;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Stop => break,
            Job::Records(records) => {
                for entry in &records {
                    let slot = (write_index as usize) % capacity as usize;
                    let offset = 4096 + slot * ENTRY_BYTES;
                    if let Some(slice) = ring.as_mut_slice() {
                        slice[offset..offset + ENTRY_BYTES].copy_from_slice(entry);
                    }
                    write_index = write_index.wrapping_add(1);
                    indexed += 1;
                }
                let heartbeat = device.heartbeat().unwrap_or(0);
                status.indexed.store(indexed, Ordering::Relaxed);
                status.heartbeat.store(heartbeat, Ordering::Relaxed);
                if let Some(slice) = ring.as_mut_slice() {
                    write_header(
                        slice,
                        &RingHeader {
                            magic: u32::from_le_bytes(*b"RTYN"),
                            version: 1,
                            capacity,
                            write_index,
                            count: indexed.min(u64::from(capacity)) as u32,
                            heartbeat: heartbeat as u32,
                            device_id: device.info.device_id,
                            clock_mhz: (device.info.clock_hz / 1_000_000) as u32,
                            indexed,
                            dropped: status.dropped.load(Ordering::Relaxed),
                            vpu_addr,
                            reserved: 0,
                        },
                    );
                }
                std::sync::atomic::fence(Ordering::SeqCst);
            }
        }
    }
    drop(queue);
    drop(ring);
    Ok(())
}

fn host_loop(rx: Receiver<Job>, status: &Status) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Stop => break,
            Job::Records(records) => {
                status.indexed.fetch_add(records.len() as u64, Ordering::Relaxed);
            }
        }
    }
}

fn maybe_submit_graph(device: &Device, queue: &device::CommandQueue, ring: &device::Buffer) {
    let Some(path) = std::env::var_os("RUSTTY_NPU_GRAPH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    match load_graph(&path) {
        Ok(commands) => match device.submit_graph(queue, &commands, ring) {
            Ok(status_code) => tracing::info!(?path, status_code, "NPU graph submitted"),
            Err(err) => tracing::warn!(%err, ?path, "failed to submit the NPU graph"),
        },
        Err(err) => tracing::warn!(%err, ?path, "NPU graph is unreadable"),
    }
}

pub(crate) fn pack_entry(line: &[Cell]) -> [u8; ENTRY_BYTES] {
    // A row is `cols` cells wide. The tail is blank padding; hashing it made a
    // fast dump spend more time on zeros than on the text that left the screen.
    let end = line
        .iter()
        .rposition(|cell| cell.cp != 0 || cell.flags != 0)
        .map_or(0, |index| index + 1);
    let hash = hash_cells(&line[..end]);
    let mut preview = [0u8; 48];
    let mut ink = 0u16;
    let mut filled = 0usize;
    let mut first = 0u32;
    for cell in &line[..end] {
        if cell.cp == 0 {
            continue;
        }
        ink = ink.saturating_add(1);
        if first == 0 {
            first = cell.cp;
        }
        if let Some(ch) = char::from_u32(cell.cp) {
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            if filled + encoded.len() <= preview.len() {
                preview[filled..filled + encoded.len()].copy_from_slice(encoded.as_bytes());
                filled += encoded.len();
            }
        }
    }
    let mut entry = [0u8; ENTRY_BYTES];
    entry[..8].copy_from_slice(&hash.to_le_bytes());
    entry[8..10].copy_from_slice(&(line.len() as u16).to_le_bytes());
    entry[10..12].copy_from_slice(&ink.to_le_bytes());
    entry[12..16].copy_from_slice(&first.to_le_bytes());
    entry[16..64].copy_from_slice(&preview);
    entry
}

fn write_header(slice: &mut [u8], header: &RingHeader) {
    let bytes = unsafe {
        std::slice::from_raw_parts((header as *const RingHeader).cast::<u8>(), HEADER_BYTES)
    };
    slice[..HEADER_BYTES].copy_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_one_cache_line() {
        assert_eq!(std::mem::size_of::<RingHeader>(), 64);
    }

    #[test]
    fn entry_packs_hash_and_preview() {
        let line = vec![Cell {
            cp: u32::from('A'),
            fg: 0,
            bg: 0,
            flags: 0,
        }];
        let entry = pack_entry(&line);
        assert_eq!(&entry[16..17], b"A");
        assert_ne!(u64::from_le_bytes(entry[..8].try_into().unwrap()), 0);
    }

    #[test]
    fn trailing_blanks_do_not_change_the_hash() {
        let glyph = Cell {
            cp: u32::from('A'),
            fg: 1,
            bg: 2,
            flags: 0,
        };
        let mut wide = vec![glyph];
        wide.resize(200, Cell::blank(0, 0));
        let wide_entry = pack_entry(&wide);
        let short_entry = pack_entry(std::slice::from_ref(&glyph));
        assert_eq!(wide_entry[..8], short_entry[..8]);
        assert_eq!(u16::from_le_bytes(wide_entry[8..10].try_into().unwrap()), 200);
        assert_eq!(&wide_entry[16..17], b"A");
    }
}
