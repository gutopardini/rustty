use crate::cell::Cell;

const MIX: u64 = 0x100000001b3;
const SEED: u64 = 0x517cc1b727220a95;

#[inline]
fn mix(hash: u64, word: u64) -> u64 {
    hash.wrapping_mul(MIX) ^ word
}

fn hash_words(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let word = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        hash = mix(hash, word);
        offset += 8;
    }
    if offset < bytes.len() {
        let mut tail = [0u8; 8];
        tail[..bytes.len() - offset].copy_from_slice(&bytes[offset..]);
        hash = mix(hash, u64::from_le_bytes(tail));
    }
    hash
}

/// Hash estável de uma linha. O caminho AVX2 carrega 32 bytes por iteração
/// e reduz na mesma ordem do escalar, para o índice da NPU não depender da CPU.
pub fn hash_cells(cells: &[Cell]) -> u64 {
    let bytes: &[u8] = bytemuck::cast_slice(cells);
    if is_x86_feature_detected!("avx2") {
        unsafe { hash_avx2(bytes) }
    } else {
        hash_words(SEED, bytes)
    }
}

#[target_feature(enable = "avx2")]
unsafe fn hash_avx2(bytes: &[u8]) -> u64 {
    use std::arch::x86_64::{_mm256_extract_epi64, _mm256_loadu_si256, __m256i};

    let mut hash = SEED;
    let mut offset = 0;
    while offset + 32 <= bytes.len() {
        let vector = unsafe { _mm256_loadu_si256(bytes[offset..].as_ptr().cast::<__m256i>()) };
        hash = mix(hash, _mm256_extract_epi64(vector, 0) as u64);
        hash = mix(hash, _mm256_extract_epi64(vector, 1) as u64);
        hash = mix(hash, _mm256_extract_epi64(vector, 2) as u64);
        hash = mix(hash, _mm256_extract_epi64(vector, 3) as u64);
        offset += 32;
    }
    hash_words(hash, &bytes[offset..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    #[test]
    fn avx2_matches_scalar_when_available() {
        let cells: Vec<Cell> = (0..40)
            .map(|i| Cell {
                cp: i,
                fg: i.wrapping_mul(3),
                bg: 0x11,
                flags: i & 7,
            })
            .collect();
        let bytes: &[u8] = bytemuck::cast_slice(&cells);
        let scalar = hash_words(SEED, bytes);
        if is_x86_feature_detected!("avx2") {
            assert_eq!(unsafe { hash_avx2(bytes) }, scalar);
        }
        assert_eq!(hash_cells(&cells), scalar);
        assert_ne!(hash_cells(&cells[..10]), hash_cells(&cells));
    }
}
