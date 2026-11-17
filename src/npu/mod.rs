//! Índice de scrollback residente no endereço virtual da NPU.
//!
//! Cada linha que sai da tela vira um registro de 64 bytes (hash AVX2 +
//! prévia). O anel fica num GEM buffer mapeado na NPU. Sem um grafo compilado
//! o firmware não é posto para calcular — o dado já está no lugar para quando
//! houver um command buffer `RTY1`.

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
    Line(Vec<Cell>),
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

    pub fn index_line(&self, line: Vec<Cell>) {
        self.indexer().index_line(line);
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
    pub fn index_line(&self, line: Vec<Cell>) {
        match self.tx.try_send(Job::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(Job::Line(_))) => {
                self.status.dropped.fetch_add(1, Ordering::Relaxed);
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
                "NPU Intel aberta"
            );
            status.present.store(true, Ordering::Relaxed);
            status.device_id.store(u64::from(device.info.device_id), Ordering::Relaxed);
            status.clock_mhz.store(device.info.clock_hz / 1_000_000, Ordering::Relaxed);
            if let Err(err) = device_loop(device, rx, &status) {
                tracing::warn!(%err, "fila da NPU caiu, índice segue na CPU");
                status.present.store(false, Ordering::Relaxed);
            }
        }
        Err(err) => {
            tracing::info!(%err, "sem NPU, índice de scrollback fica na CPU");
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
        let slice = ring.as_mut_slice().expect("anel mapeado");
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
            Job::Line(line) => {
                let entry = pack_entry(&line);
                let slot = (write_index as usize) % capacity as usize;
                let offset = 4096 + slot * ENTRY_BYTES;
                if let Some(slice) = ring.as_mut_slice() {
                    slice[offset..offset + ENTRY_BYTES].copy_from_slice(&entry);
                }
                write_index = write_index.wrapping_add(1);
                indexed += 1;
                let heartbeat = if indexed % 128 == 0 {
                    device.heartbeat().unwrap_or(0)
                } else {
                    status.heartbeat.load(Ordering::Relaxed)
                };
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
            Job::Line(line) => {
                let _ = hash_cells(&line);
                status.indexed.fetch_add(1, Ordering::Relaxed);
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
            Ok(status_code) => tracing::info!(?path, status_code, "grafo NPU submetido"),
            Err(err) => tracing::warn!(%err, ?path, "falha ao submeter grafo NPU"),
        },
        Err(err) => tracing::warn!(%err, ?path, "grafo NPU ilegível"),
    }
}

fn pack_entry(line: &[Cell]) -> [u8; ENTRY_BYTES] {
    let hash = hash_cells(line);
    let mut preview = [0u8; 48];
    let mut ink = 0u16;
    let mut filled = 0usize;
    let mut first = 0u32;
    for cell in line {
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
}
