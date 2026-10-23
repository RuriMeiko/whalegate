// Keccak-f[1600] rounds 1..23 + DeepSeekHashV1 — port 1:1 từ ds2api (CJackHwang/ds2api, pow/deepseek_hash.go)
// DeepSeekHashV1 = SHA3-256 nhưng keccak-f bỏ round 0.
// Prefix PoW = "<salt>_<expire_at_ms>_"; tìm nonce ∈ [0, difficulty) sao cho
// DeepSeekHashV1(prefix + str(nonce)) == challenge (so sánh 4 lane LE đầu).

const RC: [u64; 24] = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
];

#[inline(always)]
fn rotl64(v: u64, k: u32) -> u64 {
    v.rotate_left(k)
}

/// Keccak-f[1600] nhưng chỉ chạy rounds 1..23 (bỏ round 0) — như WASM của DeepSeek.
fn keccak_f23(s: &mut [u64; 25]) {
    let (mut a0, mut a1, mut a2, mut a3, mut a4) = (s[0], s[1], s[2], s[3], s[4]);
    let (mut a5, mut a6, mut a7, mut a8, mut a9) = (s[5], s[6], s[7], s[8], s[9]);
    let (mut a10, mut a11, mut a12, mut a13, mut a14) = (s[10], s[11], s[12], s[13], s[14]);
    let (mut a15, mut a16, mut a17, mut a18, mut a19) = (s[15], s[16], s[17], s[18], s[19]);
    let (mut a20, mut a21, mut a22, mut a23, mut a24) = (s[20], s[21], s[22], s[23], s[24]);

    for r in 1..24 {
        // theta
        let c0 = a0 ^ a5 ^ a10 ^ a15 ^ a20;
        let c1 = a1 ^ a6 ^ a11 ^ a16 ^ a21;
        let c2 = a2 ^ a7 ^ a12 ^ a17 ^ a22;
        let c3 = a3 ^ a8 ^ a13 ^ a18 ^ a23;
        let c4 = a4 ^ a9 ^ a14 ^ a19 ^ a24;
        let d0 = c4 ^ rotl64(c1, 1);
        let d1 = c0 ^ rotl64(c2, 1);
        let d2 = c1 ^ rotl64(c3, 1);
        let d3 = c2 ^ rotl64(c4, 1);
        let d4 = c3 ^ rotl64(c0, 1);
        a0 ^= d0; a5 ^= d0; a10 ^= d0; a15 ^= d0; a20 ^= d0;
        a1 ^= d1; a6 ^= d1; a11 ^= d1; a16 ^= d1; a21 ^= d1;
        a2 ^= d2; a7 ^= d2; a12 ^= d2; a17 ^= d2; a22 ^= d2;
        a3 ^= d3; a8 ^= d3; a13 ^= d3; a18 ^= d3; a23 ^= d3;
        a4 ^= d4; a9 ^= d4; a14 ^= d4; a19 ^= d4; a24 ^= d4;

        // rho + pi
        let b0 = a0;
        let b10 = rotl64(a1, 1);
        let b20 = rotl64(a2, 62);
        let b5 = rotl64(a3, 28);
        let b15 = rotl64(a4, 27);
        let b16 = rotl64(a5, 36);
        let b1 = rotl64(a6, 44);
        let b11 = rotl64(a7, 6);
        let b21 = rotl64(a8, 55);
        let b6 = rotl64(a9, 20);
        let b7 = rotl64(a10, 3);
        let b17 = rotl64(a11, 10);
        let b2 = rotl64(a12, 43);
        let b12 = rotl64(a13, 25);
        let b22 = rotl64(a14, 39);
        let b23 = rotl64(a15, 41);
        let b8 = rotl64(a16, 45);
        let b18 = rotl64(a17, 15);
        let b3 = rotl64(a18, 21);
        let b13 = rotl64(a19, 8);
        let b14 = rotl64(a20, 18);
        let b24 = rotl64(a21, 2);
        let b9 = rotl64(a22, 61);
        let b19 = rotl64(a23, 56);
        let b4 = rotl64(a24, 14);

        // chi
        a0 = b0 ^ (!b1 & b2);
        a1 = b1 ^ (!b2 & b3);
        a2 = b2 ^ (!b3 & b4);
        a3 = b3 ^ (!b4 & b0);
        a4 = b4 ^ (!b0 & b1);
        a5 = b5 ^ (!b6 & b7);
        a6 = b6 ^ (!b7 & b8);
        a7 = b7 ^ (!b8 & b9);
        a8 = b8 ^ (!b9 & b5);
        a9 = b9 ^ (!b5 & b6);
        a10 = b10 ^ (!b11 & b12);
        a11 = b11 ^ (!b12 & b13);
        a12 = b12 ^ (!b13 & b14);
        a13 = b13 ^ (!b14 & b10);
        a14 = b14 ^ (!b10 & b11);
        a15 = b15 ^ (!b16 & b17);
        a16 = b16 ^ (!b17 & b18);
        a17 = b17 ^ (!b18 & b19);
        a18 = b18 ^ (!b19 & b15);
        a19 = b19 ^ (!b15 & b16);
        a20 = b20 ^ (!b21 & b22);
        a21 = b21 ^ (!b22 & b23);
        a22 = b22 ^ (!b23 & b24);
        a23 = b23 ^ (!b24 & b20);
        a24 = b24 ^ (!b20 & b21);

        // iota
        a0 ^= RC[r];
    }

    s[0] = a0; s[1] = a1; s[2] = a2; s[3] = a3; s[4] = a4;
    s[5] = a5; s[6] = a6; s[7] = a7; s[8] = a8; s[9] = a9;
    s[10] = a10; s[11] = a11; s[12] = a12; s[13] = a13; s[14] = a14;
    s[15] = a15; s[16] = a16; s[17] = a17; s[18] = a18; s[19] = a19;
    s[20] = a20; s[21] = a21; s[22] = a22; s[23] = a23; s[24] = a24;
}

#[inline]
fn absorb_block(s: &mut [u64; 25], block: &[u8]) {
    for i in 0..17 {
        let mut lane = 0u64;
        for b in 0..8 {
            lane |= (block[i * 8 + b] as u64) << (8 * b);
        }
        s[i] ^= lane;
    }
}

/// DeepSeekHashV1: sponge SHA3-256 (rate 136, pad 0x06/0x80) với keccak-f rounds 1..23
pub fn deepseek_hash_v1(data: &[u8]) -> [u8; 32] {
    const RATE: usize = 136;
    let mut s = [0u64; 25];

    let mut off = 0;
    while off + RATE <= data.len() {
        absorb_block(&mut s, &data[off..off + RATE]);
        keccak_f23(&mut s);
        off += RATE;
    }

    let mut block = [0u8; RATE];
    block[..data.len() - off].copy_from_slice(&data[off..]);
    block[data.len() - off] = 0x06;
    block[RATE - 1] |= 0x80;
    absorb_block(&mut s, &block);
    keccak_f23(&mut s);

    let mut out = [0u8; 32];
    for i in 0..4 {
        out[i * 8..(i + 1) * 8].copy_from_slice(&s[i].to_le_bytes());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Prefix = "<salt>_<expire_at_ms>_" (expire_at GIỮ NGUYÊN giá trị ms server trả về)
pub fn build_prefix(salt: &str, expire_at: u64) -> String {
    format!("{}_{}_", salt, expire_at)
}

/// Tìm nonce ∈ [0, difficulty) sao cho DeepSeekHashV1(prefix + nonce) == challenge.
pub fn solve_pow(challenge_hex: &str, salt: &str, expire_at: u64, difficulty: u64) -> Option<u64> {
    if challenge_hex.len() != 64 {
        return None;
    }
    let prefix = build_prefix(salt, expire_at);
    let pb = prefix.as_bytes();
    for nonce in 0..difficulty {
        let mut input = pb.to_vec();
        input.extend_from_slice(nonce.to_string().as_bytes());
        if hex(&deepseek_hash_v1(&input)) == challenge_hex {
            return Some(nonce);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pow_solve_reference_vector() {
        // Vector tự sinh: chọn nonce=12345, tính challenge, rồi cho solver tìm lại
        let salt = "2b2d8131db1c6ea73037";
        let expire_at: u64 = 1792655686557;
        let nonce: u64 = 12345;
        let prefix = build_prefix(salt, expire_at);
        let input = format!("{}{}", prefix, nonce);
        let challenge = hex(&deepseek_hash_v1(input.as_bytes()));
        assert_eq!(solve_pow(&challenge, salt, expire_at, 200_000), Some(nonce));
    }

    #[test]
    fn test_prefix_format() {
        assert_eq!(build_prefix("abc", 1792655686557), "abc_1792655686557_");
    }
}
