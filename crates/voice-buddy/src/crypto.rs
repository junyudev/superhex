//! secp128r1 is a protocol constraint. Fixed-width field arithmetic and
//! branch-free point selection keep ephemeral scalar bits out of control flow.
use crate::{Failure, wire::malformed};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use crypto_bigint::{Encoding, U128, impl_modulus, modular::ConstMontyForm};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};
use zeroize::{Zeroize, Zeroizing};

impl_modulus!(Prime, U128, "FFFFFFFDFFFFFFFFFFFFFFFFFFFFFFFF");
type F = ConstMontyForm<Prime, { U128::LIMBS }>;
const P: U128 = U128::from_be_hex("FFFFFFFDFFFFFFFFFFFFFFFFFFFFFFFF");
const N: U128 = U128::from_be_hex("FFFFFFFE0000000075A30D1B9038A115");
const B: U128 = U128::from_be_hex("E87579C11079F43DD824993C2CEE5ED3");
const GX: U128 = U128::from_be_hex("161FF7528B899B2D0C28607CA52C5B86");
const GY: U128 = U128::from_be_hex("CF5AC8395BAFEB13C02DA292DDED7A83");

#[derive(Clone, Copy)]
struct Point {
    x: F,
    y: F,
    z: F,
}
impl Point {
    fn infinity() -> Self {
        Self {
            x: F::ZERO,
            y: F::ONE,
            z: F::ZERO,
        }
    }
    fn affine(x: U128, y: U128) -> Self {
        Self {
            x: F::new(&x),
            y: F::new(&y),
            z: F::ONE,
        }
    }
    fn select(a: Self, b: Self, choice: Choice) -> Self {
        Self {
            x: F::conditional_select(&a.x, &b.x, choice),
            y: F::conditional_select(&a.y, &b.y, choice),
            z: F::conditional_select(&a.z, &b.z, choice),
        }
    }
    fn double(self) -> Self {
        let delta = self.z.square();
        let gamma = self.y.square();
        let beta = self.x * gamma;
        let three = F::new(&U128::from(3u8));
        let four = F::new(&U128::from(4u8));
        let eight = F::new(&U128::from(8u8));
        let alpha = three * (self.x - delta) * (self.x + delta);
        let x = alpha.square() - eight * beta;
        let z = (self.y + self.z).square() - gamma - delta;
        Self {
            x,
            y: alpha * (four * beta - x) - eight * gamma.square(),
            z,
        }
    }
    fn add(self, other: Self) -> Self {
        let z1 = self.z.square();
        let z2 = other.z.square();
        let u1 = self.x * z2;
        let u2 = other.x * z1;
        let s1 = self.y * other.z * z2;
        let s2 = other.y * self.z * z1;
        let h = u2 - u1;
        let i = (h + h).square();
        let j = h * i;
        let r = (s2 - s1) + (s2 - s1);
        let v = u1 * i;
        let x = r.square() - j - v - v;
        let s1j = s1 * j;
        let sum = Self {
            x,
            y: r * (v - x) - s1j - s1j,
            z: ((self.z + other.z).square() - z1 - z2) * h,
        };
        let sum = Self::select(sum, self.double(), u1.ct_eq(&u2) & s1.ct_eq(&s2));
        let sum = Self::select(sum, other, self.z.ct_eq(&F::ZERO));
        Self::select(sum, self, other.z.ct_eq(&F::ZERO))
    }
    fn multiply(self, scalar: &[u8; 16]) -> Self {
        let mut point = Self::infinity();
        for byte in scalar {
            for bit in (0..8).rev() {
                let doubled = point.double();
                point = Self::select(doubled, doubled.add(self), Choice::from((byte >> bit) & 1));
            }
        }
        point
    }
    fn coordinates(self) -> Result<([u8; 16], [u8; 16]), Failure> {
        if bool::from(self.z.ct_eq(&F::ZERO)) {
            return Err(malformed());
        }
        let inverse = self
            .z
            .pow(&U128::from_be_hex("FFFFFFFDFFFFFFFFFFFFFFFFFFFFFFFD"));
        let z2 = inverse.square();
        Ok((
            (self.x * z2).retrieve().to_be_bytes(),
            (self.y * z2 * inverse).retrieve().to_be_bytes(),
        ))
    }
}
impl Drop for KeyExchange {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}
pub(crate) struct KeyExchange {
    private: [u8; 16],
}
impl KeyExchange {
    pub fn new() -> Result<Self, Failure> {
        loop {
            let mut private = Zeroizing::new([0; 16]);
            random(private.as_mut())?;
            let n = U128::from_be_bytes(*private);
            if n != U128::ZERO && n < N {
                return Ok(Self { private: *private });
            }
        }
    }
    pub fn public(&self) -> Result<String, Failure> {
        let (x, y) = Point::affine(GX, GY)
            .multiply(&self.private)
            .coordinates()?;
        Ok(format!(
            "04{}{}",
            hex::encode_upper(x),
            hex::encode_upper(y)
        ))
    }
    pub fn shared(&self, server: &str) -> Result<Zeroizing<String>, Failure> {
        if server.len() != 66 {
            return Err(malformed());
        }
        let bytes = hex::decode(server).map_err(|_| malformed())?;
        if bytes[0] != 4 {
            return Err(malformed());
        }
        let x = U128::from_be_slice(&bytes[1..17]);
        let y = U128::from_be_slice(&bytes[17..33]);
        if x >= P || y >= P {
            return Err(malformed());
        }
        let point = Point::affine(x, y);
        if point.y.square() != point.x.square() * point.x - point.x - point.x - point.x + F::new(&B)
        {
            return Err(malformed());
        }
        // secp128r1 has cofactor one: a finite point on the curve is in its prime-order group.
        let (mut shared, mut other) = point.multiply(&self.private).coordinates()?;
        let key = Zeroizing::new(hex::encode_upper(shared));
        shared.zeroize();
        other.zeroize();
        Ok(key)
    }
}
pub(crate) fn random(bytes: &mut [u8]) -> Result<(), Failure> {
    getrandom::fill(bytes).map_err(|_| Failure::Internal("random source failed".into()))
}
pub(crate) fn encrypt(body: &[u8], key: &str) -> Result<Vec<u8>, Failure> {
    macro_rules! enc {
        ($aes:ty) => {
            ecb::Encryptor::<$aes>::new_from_slice(key.as_bytes())
                .map_err(|_| malformed())?
                .encrypt_padded_vec_mut::<Pkcs7>(body)
        };
    }
    Ok(match key.len() {
        16 => enc!(aes::Aes128),
        24 => enc!(aes::Aes192),
        32 => enc!(aes::Aes256),
        _ => return Err(malformed()),
    })
}
pub(crate) fn decrypt(body: &[u8], key: &str) -> Result<Vec<u8>, Failure> {
    macro_rules! dec {
        ($aes:ty) => {
            ecb::Decryptor::<$aes>::new_from_slice(key.as_bytes())
                .map_err(|_| malformed())?
                .decrypt_padded_vec_mut::<Pkcs7>(body)
                .map_err(|_| malformed())?
        };
    }
    Ok(match key.len() {
        16 => dec!(aes::Aes128),
        24 => dec!(aes::Aes192),
        32 => dec!(aes::Aes256),
        _ => return Err(malformed()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Generated by Python ecdsa.SECP128r1; these are invented test scalars.
    #[test]
    fn python_vectors() {
        include!("vectors.in");
    }
    #[test]
    fn rejects_invalid_server_points() {
        let key = KeyExchange { private: [1; 16] };
        for bad in [
            String::new(),
            "00".repeat(33),
            format!("04{}{}", "FF".repeat(16), "00".repeat(16)),
            format!("04{}{}", "00".repeat(16), "00".repeat(16)),
        ] {
            assert!(key.shared(&bad).is_err());
        }
    }
}
