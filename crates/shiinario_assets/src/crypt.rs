// Ported from GARbro WarcEncryption.cs, Copyright (C) 2015-2017 morkt (MIT).
use crate::profile::Profile;
use std::f64::consts::PI;
pub const MAX_INDEX: usize = (32 + 24) * 16384;
struct Random(u32);
impl Random {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1566083941).wrapping_add(1);
        self.0
    }
    fn unit(&mut self) -> f64 {
        self.next() as f64 / 4294967296.0
    }
    fn helper2(&mut self, a: f64) -> u32 {
        let v3;
        if a > 1.0 {
            let v0 = (a * 2.0 - 1.0).sqrt();
            loop {
                let v1 = 1.0 - self.unit();
                let mut v2 = 2.0 * self.unit() - 1.0;
                if v1 * v1 + v2 * v2 > 1.0 {
                    continue;
                }
                v2 /= v1;
                let candidate = v2 * v0 + a - 1.0;
                if candidate <= 0.0 {
                    continue;
                }
                let v1 = (a - 1.0) * (candidate / (a - 1.0)).ln() - v2 * v0;
                if v1 < -50.0 {
                    continue;
                }
                if self.unit() <= v1.exp() * (v2 * v2 + 1.0) {
                    v3 = candidate;
                    break;
                }
            }
        } else {
            let v0 = 1.0f64.exp() / (a + 1.0f64.exp());
            loop {
                let mut v1 = self.unit();
                let v2 = self.unit();
                let candidate;
                if v1 < v0 {
                    candidate = v2.powf(1.0 / a);
                    v1 = (-candidate).exp();
                } else {
                    candidate = 1.0 - v2.ln();
                    v1 = candidate.powf(a - 1.0);
                }
                if self.unit() < v1 {
                    v3 = candidate;
                    break;
                }
            }
        }
        (v3 * 256.0) as u32
    }
}
fn helper1(a: f64) -> f64 {
    if a < 0.0 {
        return -helper1(-a);
    }
    if a < 18.0 {
        let mut v0 = a;
        let mut v1 = a;
        let v2 = -(a * a);
        for j in (3..1000).step_by(2) {
            v1 *= v2 / (j * (j - 1)) as f64;
            v0 += v1 / j as f64;
            if v0 == v2 {
                break;
            }
        }
        return v0;
    }
    let (mut flags, mut v0, mut v1, mut v0_l, mut v1_l, mut v0_h, mut v1_h) =
        (0, 0.0, 0.0, 0.0, 0.0, 2.0, 2.0);
    let mut div = 1.0 / a;
    let mut i = 0;
    loop {
        v0 += div;
        i += 1;
        div *= i as f64 / a;
        if v0 < v0_h {
            v0_h = v0;
        } else {
            flags |= 1;
        }
        v1 += div;
        i += 1;
        div *= i as f64 / a;
        if v1 < v1_h {
            v1_h = v1;
        } else {
            flags |= 2;
        }
        v0 -= div;
        i += 1;
        div *= i as f64 / a;
        if v0 > v0_l {
            v0_l = v0;
        } else {
            flags |= 4;
        }
        v1 -= div;
        i += 1;
        div *= i as f64 / a;
        if v1 > v1_l {
            v1_l = v1;
        } else {
            flags |= 8;
        }
        if flags == 15 {
            break;
        }
    }
    ((PI - a.cos() * (v0_l + v0_h)) - (a.sin() * (v1_l + v1_h))) / 2.0
}
fn helper3(key: u32) -> u32 {
    let b = key.to_le_bytes();
    let f = |i: usize| (1.5 * b[i] as f64 + 0.1) as f32;
    let v0 = f(0).to_bits().swap_bytes();
    let v1 = f(1) as u32;
    let v2 = f(2).to_bits().wrapping_neg();
    let v3 = !f(3).to_bits();
    v0.wrapping_add(v1) | v2.wrapping_sub(v3)
}
fn region_crc(region: &[u8], mut flags: u32, rgb: u32) -> u32 {
    let mut sa = (flags & 511) as i32;
    let mut da = ((flags >> 12) & 511) as i32;
    flags >>= 24;
    if flags & 16 == 0 {
        da = 0;
    }
    if flags & 8 == 0 {
        sa = 256;
    }
    let (mut pos, mut xs, mut ys) = (0i32, 4i32, 0i32);
    if flags & 64 != 0 {
        ys += 48;
        pos += 47 * 4;
        xs = -xs;
    }
    if flags & 32 != 0 {
        ys -= 48;
        pos += 48 * 47 * 4;
    }
    ys <<= 3;
    let mut sum = 0u32;
    for _ in 0..48 {
        for _ in 0..48 {
            let alpha = (region[pos as usize + 3] as i32 * sa) >> 8;
            let mut color = rgb;
            for i in 0..3 {
                let v = region[pos as usize + i] as i32;
                let c = (((((((color & 255) as i32 - v) * da) >> 8) + v) & 255) * alpha) >> 8;
                let mut poly = (c as u32 ^ sum) & 255;
                for _ in 0..8 {
                    let bit = poly & 1;
                    poly = poly.rotate_right(1);
                    if bit == 0 {
                        poly ^= 0x6db88320;
                    }
                }
                sum = (sum >> 8) ^ poly;
                color >>= 8;
            }
            pos += xs;
        }
        pos += ys;
    }
    sum
}
// Gregorian conversion for Windows FILETIME, including years beyond chrono's range.
fn filetime(t: u64) -> [u32; 3] {
    let ms = t / 10000;
    let days = (ms / 86400000) as i64;
    let z = days - 134774 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = mp + if mp < 10 { 3 } else { -9 };
    if m <= 2 {
        y += 1;
    }
    [
        (y as u32) | (m as u32) << 16,
        ((ms / 3600000) % 24) as u32 | (((ms / 60000) % 60) as u32) << 16,
        ((ms / 1000) % 60) as u32 | ((ms % 1000) as u32) << 16,
    ]
}
fn helper4(profile: &Profile, data: &mut [u8]) {
    let mut buf = [0u32; 80];
    for (i, b) in buf[..16].iter_mut().enumerate() {
        *b = u32::from_be_bytes(data[40 + i * 4..44 + i * 4].try_into().unwrap());
    }
    for i in 16..80 {
        buf[i] = (buf[i - 16] ^ buf[i - 14] ^ buf[i - 8] ^ buf[i - 3]).rotate_left(1);
    }
    let mut key = [0u32; 10];
    key[..5].copy_from_slice(&profile.helper_key);
    let [mut k0, mut k1, mut k2, mut k3, mut k4] = key[..5].try_into().unwrap();
    for (i, b) in buf.iter().enumerate() {
        let (f, c) = match i {
            0..16 => (k1 ^ k2 ^ k3, 0u32),
            16..32 => (k1 & k2 | k3 & !k1, 0x5a827999),
            32..48 => (k3 ^ (k1 | !k2), 0x6ed9eba1),
            48..64 => (k1 & k3 | k2 & !k3, 0x8f1bbcdc),
            _ => (k1 ^ (k2 | !k3), 0xa953fd4e),
        };
        let next = b
            .wrapping_add(k4)
            .wrapping_add(f)
            .wrapping_add(c)
            .wrapping_add(k0.rotate_left(5));
        (k0, k1, k2, k3, k4) = (next, k0, k1.rotate_right(2), k2, k3);
    }
    for (v, k) in [k0, k1, k2, k3, k4].into_iter().zip(&mut key) {
        *k = k.wrapping_add(v);
    }
    let time = filetime(((key[0] & 0x7fffffff) as u64) << 32 | key[1] as u64);
    key[5] = time[0];
    key[7] = time[1];
    key[8] = time[2];
    let mut flags = u32::from_le_bytes(data[40..44].try_into().unwrap()) | 0x80000000;
    if flags & 0x78000000 == 0 {
        flags |= 0x98000000;
    }
    key[9] = ((key[2] as i32 as i64 * key[3] as i32 as i64) >> 8) as u32;
    key[6] = region_crc(&profile.region, flags, buf[1] >> 8).wrapping_add(key[9]);
    for (chunk, k) in data[..40].as_chunks_mut::<4>().0.iter_mut().zip(key) {
        for (d, b) in chunk.iter_mut().zip(k.to_le_bytes()) {
            *d ^= b;
        }
    }
}
pub fn decrypt(profile: &Profile, data: &mut [u8]) {
    let len = data.len();
    if len < 3 {
        return;
    }
    let mut effective = len.min(1024);
    let mut offset = 0;
    let a = (data[0] as i8 ^ len as i8) as i32;
    let b = (data[1] as i8 ^ (len / 2) as i8) as i32;
    let mut rng = Random(len as u32);
    let mut fac = 0;
    if len != MAX_INDEX {
        let idx = (rng.next() as f64 * (profile.image.len() as f64 / 4294967296.0)) as usize;
        fac = helper3(rng.0.wrapping_add(profile.image[idx] as u32)) & 0xfffffff;
        if effective > 128 {
            helper4(profile, &mut data[4..]);
            offset = 128;
            effective -= 128;
        }
    }
    // C#'s signed floating point conversion followed by unsigned wrap.
    rng.0 ^= (helper1(a as f64) * 100000000.0) as i64 as u32;
    let mut token = if a | b == 0 {
        0.0
    } else {
        (a as f64 / ((a * a + b * b) as f64).sqrt()).acos() / PI * 180.0
    };
    if b < 0 {
        token = 360.0 - token;
    }
    let mut x =
        (fac.wrapping_add(rng.helper2(token) as u8 as u32) % profile.key.len() as u32) as usize;
    for i in 2..effective {
        let d = (data[offset + i] ^ (rng.next() >> 24) as u8).rotate_right(1)
            ^ profile.key[(i - 2) % profile.key.len()]
            ^ profile.key[x];
        data[offset + i] = d;
        x = d as usize % profile.key.len();
    }
}
pub fn decrypt_index(profile: &Profile, offset: u32, data: &mut [u8]) {
    decrypt(profile, data);
    let key = offset.to_le_bytes();
    for (i, d) in data.iter_mut().enumerate() {
        *d ^= key[i % 4] ^ !170u8;
    }
}
pub fn decrypt2(profile: &Profile, data: &mut [u8]) {
    if data.len() < 1024 {
        return;
    }
    let mut crc = 0xffffffffu32;
    for &b in &data[..256] {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x80000000 != 0 {
                (crc << 1) ^ 0x04c11db7
            } else {
                crc << 1
            };
        }
    }
    for i in (256..512).step_by(4) {
        let src = (u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) & 0x1ffc) as usize;
        let k = u32::from_le_bytes(profile.decode[src..src + 4].try_into().unwrap()) ^ crc;
        for (d, b) in data[i + 256..i + 260].iter_mut().zip(k.to_le_bytes()) {
            *d ^= b;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_epoch() {
        assert_eq!(filetime(0), [1601 | 1 << 16, 0, 0]);
        assert_eq!(filetime(116444736000000000), [1970 | 1 << 16, 0, 0]);
    }
}
