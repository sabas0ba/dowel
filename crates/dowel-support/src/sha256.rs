//! SHA-256。取得したものが宣言どおりかを確かめるために要る。
//!
//! 自前で持つのは、標準ライブラリに無く、外部コマンドが環境で違うためである
//! （GNU は `sha256sum`、macOS は `shasum -a 256`、Windows は別物）。
//! 「取ってきたものを検める」手続きが環境によって在ったり無かったりするのは、
//! 固定の意味を薄める——`git` に委譲できるのは、取得と検証が同じ道具の中で
//! 閉じているからである（[ADR-0029](../../../docs/adr/0029-tarball-dependencies.md)）。
//!
//! 実装は FIPS 180-4 のまま。暗号用途の定数時間性は要らない——比べる相手は
//! マニフェストに書かれた公開の値であり、秘密ではない。

/// 丸め定数。立方根の小数部の上位32ビット（FIPS 180-4）。
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// 初期値。平方根の小数部の上位32ビット。
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// 逐次に食わせる計算器。
///
/// 一度に読める大きさの塊とは限らない——tarball は数十メガバイトになる。
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    /// 未処理の入力。64 バイト揃うたびに畳む
    buf: [u8; 64],
    buf_len: usize,
    /// 食わせた総バイト数。末尾に長さを書くために要る
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Sha256 {
        Sha256::new()
    }
}

impl Sha256 {
    pub fn new() -> Sha256 {
        Sha256 { h: H0, buf: [0; 64], buf_len: 0, total: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        // 手元に半端が在れば、まず 64 バイトまで埋める。
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 64 {
                return;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        // 揃っている分は写さずに畳む。
        let mut chunks = data.chunks_exact(64);
        for block in &mut chunks {
            let mut b = [0u8; 64];
            b.copy_from_slice(block);
            self.compress(&b);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    /// 32 バイトのダイジェスト。
    pub fn finish(mut self) -> [u8; 32] {
        // 詰め物: `0x80`、0 の並び、最後に総ビット数を 64 ビットの大端で。
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        // `update` が総数を進めてしまうので、詰め物の長さは記録前の値で決める。
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        let block = {
            let mut b = self.buf;
            b[56..].copy_from_slice(&bits.to_be_bytes());
            b
        };
        self.compress(&block);

        let mut out = [0u8; 32];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, src) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *dst = dst.wrapping_add(src);
        }
    }
}

/// 16 進の小文字で綴ったダイジェスト。
pub fn hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// バイト列のダイジェストを 16 進で。
pub fn hex_of(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finish())
}

/// ファイルのダイジェストを 16 進で。
///
/// 全体を記憶に載せない。tarball は数十メガバイトになりうる。
pub fn hex_of_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 と RFC 6234 の試験ベクタ。
    #[test]
    fn the_published_vectors_match() {
        assert_eq!(hex_of(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 56 バイト。詰め物が次の塊へ溢れる境目である。
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 2つの塊に跨る。
        assert_eq!(
            hex_of(&b"a".repeat(1_000_000)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn feeding_in_pieces_gives_the_same_answer() {
        // tarball は一度に読めない。分けて食わせても答が変わらないこと。
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let whole = hex_of(&data);
        for chunk in [1usize, 7, 63, 64, 65, 1000] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(hex(&h.finish()), whole, "chunk size {chunk}");
        }
    }

    #[test]
    fn a_file_hashes_the_same_as_its_bytes() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sha256-file");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(hex_of_file(&p).unwrap(), hex_of(&data));
    }
}
