//! Minimal secp160r1 ECDH. No maintained Rust crate covers this curve, and the
//! device handshake requires it. Operations are not constant-time; this is a local
//! status reader talking to a device the user owns, not a general crypto library.

use num_bigint::BigUint;
use num_traits::{One, Zero};
use rand::Rng;
use zeroize::Zeroizing;

const COORD_LEN: usize = 20;

struct Curve {
    p: BigUint,
    a: BigUint,
    b: BigUint,
    gx: BigUint,
    gy: BigUint,
    n: BigUint,
}

fn hex(s: &str) -> BigUint {
    BigUint::parse_bytes(s.as_bytes(), 16).expect("valid hex constant")
}

fn curve() -> Curve {
    Curve {
        p: hex("ffffffffffffffffffffffffffffffff7fffffff"),
        a: hex("ffffffffffffffffffffffffffffffff7ffffffc"),
        b: hex("1c97befc54bd7a8b65acf89f81d4d4adc565fa45"),
        gx: hex("4a96b5688ef573284664698968c38bb913cbfc82"),
        gy: hex("23a628553168947d59dcc912042351377ac5fb32"),
        n: hex("0100000000000000000001f4c8f927aed3ca752257"),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EcdhError {
    #[error("public key must be 40 bytes, got {0}")]
    BadLength(usize),
    #[error("public key is not a point on secp160r1")]
    NotOnCurve,
    #[error("shared secret is the point at infinity")]
    Infinity,
}

/// Jacobian point; `z == 0` is infinity.
#[derive(Clone)]
struct Jac {
    x: BigUint,
    y: BigUint,
    z: BigUint,
}

impl Jac {
    fn infinity() -> Self {
        Self {
            x: BigUint::one(),
            y: BigUint::one(),
            z: BigUint::zero(),
        }
    }
    fn is_infinity(&self) -> bool {
        self.z.is_zero()
    }
}

fn sub(a: &BigUint, b: &BigUint, p: &BigUint) -> BigUint {
    ((a % p) + p - (b % p)) % p
}

fn double(c: &Curve, pt: &Jac) -> Jac {
    if pt.is_infinity() || pt.y.is_zero() {
        return Jac::infinity();
    }
    let p = &c.p;
    // a = -3 formulas (dbl-2001-b).
    let delta = (&pt.z * &pt.z) % p;
    let gamma = (&pt.y * &pt.y) % p;
    let beta = (&pt.x * &gamma) % p;
    let alpha = (BigUint::from(3u8) * sub(&pt.x, &delta, p) * ((&pt.x + &delta) % p)) % p;
    let x3 = sub(&((&alpha * &alpha) % p), &((BigUint::from(8u8) * &beta) % p), p);
    let yz = (&pt.y + &pt.z) % p;
    let z3 = sub(&sub(&((&yz * &yz) % p), &gamma, p), &delta, p);
    let y3 = sub(
        &((&alpha * sub(&((BigUint::from(4u8) * &beta) % p), &x3, p)) % p),
        &((BigUint::from(8u8) * &gamma * &gamma) % p),
        p,
    );
    Jac { x: x3, y: y3, z: z3 }
}

/// Mixed addition: Jacobian + affine (madd-2007-bl).
fn add_affine(c: &Curve, pt: &Jac, qx: &BigUint, qy: &BigUint) -> Jac {
    let p = &c.p;
    if pt.is_infinity() {
        return Jac {
            x: qx.clone(),
            y: qy.clone(),
            z: BigUint::one(),
        };
    }
    let z1z1 = (&pt.z * &pt.z) % p;
    let u2 = (qx * &z1z1) % p;
    let s2 = (qy * &pt.z % p * &z1z1) % p;
    let h = sub(&u2, &pt.x, p);
    let r2 = sub(&s2, &pt.y, p);
    if h.is_zero() {
        return if r2.is_zero() { double(c, pt) } else { Jac::infinity() };
    }
    let hh = (&h * &h) % p;
    let i = (BigUint::from(4u8) * &hh) % p;
    let j = (&h * &i) % p;
    let r = (BigUint::from(2u8) * r2) % p;
    let v = (&pt.x * &i) % p;
    let x3 = sub(&sub(&((&r * &r) % p), &j, p), &((BigUint::from(2u8) * &v) % p), p);
    let y3 = sub(
        &((&r * sub(&v, &x3, p)) % p),
        &((BigUint::from(2u8) * &pt.y % p * &j) % p),
        p,
    );
    let zh = (&pt.z + &h) % p;
    let z3 = sub(&sub(&((&zh * &zh) % p), &z1z1, p), &hh, p);
    Jac { x: x3, y: y3, z: z3 }
}

fn mul(c: &Curve, k: &BigUint, qx: &BigUint, qy: &BigUint) -> Jac {
    let mut acc = Jac::infinity();
    for i in (0..k.bits()).rev() {
        acc = double(c, &acc);
        if k.bit(i) {
            acc = add_affine(c, &acc, qx, qy);
        }
    }
    acc
}

fn to_affine(c: &Curve, pt: &Jac) -> Option<(BigUint, BigUint)> {
    if pt.is_infinity() {
        return None;
    }
    let p = &c.p;
    let zinv = pt.z.modpow(&(p - 2u8), p);
    let zinv2 = (&zinv * &zinv) % p;
    let zinv3 = (&zinv2 * &zinv) % p;
    Some(((&pt.x * zinv2) % p, (&pt.y * zinv3) % p))
}

fn is_on_curve(c: &Curve, x: &BigUint, y: &BigUint) -> bool {
    let p = &c.p;
    if x >= p || y >= p {
        return false;
    }
    let lhs = (y * y) % p;
    let rhs = ((x * x % p * x) + (&c.a * x) + &c.b) % p;
    lhs == rhs
}

fn pad(v: &BigUint) -> [u8; COORD_LEN] {
    let bytes = v.to_bytes_be();
    let mut out = [0u8; COORD_LEN];
    out[COORD_LEN - bytes.len()..].copy_from_slice(&bytes);
    out
}

pub struct KeyPair {
    secret: Zeroizing<Vec<u8>>,
    public: [u8; 2 * COORD_LEN],
}

impl KeyPair {
    pub fn generate() -> Self {
        let c = curve();
        let mut rng = rand::rng();
        let mut raw = vec![0u8; COORD_LEN + 1];
        rng.fill_bytes(&mut raw);
        let k = BigUint::from_bytes_be(&raw) % (&c.n - 1u8) + 1u8;
        raw.iter_mut().for_each(|b| *b = 0);
        Self::from_secret(&k)
    }

    fn from_secret(k: &BigUint) -> Self {
        let c = curve();
        let (x, y) = to_affine(&c, &mul(&c, k, &c.gx, &c.gy)).expect("k in [1, n-1]");
        let mut public = [0u8; 2 * COORD_LEN];
        public[..COORD_LEN].copy_from_slice(&pad(&x));
        public[COORD_LEN..].copy_from_slice(&pad(&y));
        Self {
            secret: Zeroizing::new(k.to_bytes_be()),
            public,
        }
    }

    /// Test helper: deterministic key from a big-endian secret.
    #[cfg(test)]
    pub fn from_secret_bytes(secret: &[u8]) -> Self {
        Self::from_secret(&BigUint::from_bytes_be(secret))
    }

    /// `x || y`, 20 bytes each, big-endian.
    pub fn public_bytes(&self) -> [u8; 2 * COORD_LEN] {
        self.public
    }

    /// ECDH: returns the 20-byte big-endian x coordinate of `secret * peer`.
    pub fn shared_secret(&self, peer: &[u8]) -> Result<Zeroizing<[u8; COORD_LEN]>, EcdhError> {
        if peer.len() != 2 * COORD_LEN {
            return Err(EcdhError::BadLength(peer.len()));
        }
        let c = curve();
        let x = BigUint::from_bytes_be(&peer[..COORD_LEN]);
        let y = BigUint::from_bytes_be(&peer[COORD_LEN..]);
        if !is_on_curve(&c, &x, &y) {
            return Err(EcdhError::NotOnCurve);
        }
        let k = BigUint::from_bytes_be(&self.secret);
        let (sx, _) = to_affine(&c, &mul(&c, &k, &x, &y)).ok_or(EcdhError::Infinity)?;
        Ok(Zeroizing::new(pad(&sx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors generated with python-ecdsa (SECP160r1).
    const SK1: &str = "1234567890abcdef1234567890abcdef12345678";
    const SK2: &str = "abcdef0123456789abcdef0123456789abcdef01";
    const PK1: &str = "d463026b5115d49f639b1bb411b9a9af37aa79bef17c1e630abccc30e297d91d00ac4522cbc1f0fa";
    const PK2: &str = "9f0e3b4415d721d7d6195d22395a190e31d72bd1de02414bce02dc5abd56dcf8fc66cbba98dfa136";
    const SHARED: &str = "0c55fff4fe71ca511e0eb24cd1ab7f44fd641f27";

    #[test]
    fn generator_is_on_curve_and_has_order_n() {
        let c = curve();
        assert!(is_on_curve(&c, &c.gx, &c.gy));
        // (n-1)*G + G == infinity
        let nm1 = &c.n - 1u8;
        let (x, y) = to_affine(&c, &mul(&c, &nm1, &c.gx, &c.gy)).unwrap();
        assert!(
            add_affine(
                &c,
                &Jac {
                    x,
                    y,
                    z: BigUint::one()
                },
                &c.gx,
                &c.gy
            )
            .is_infinity()
        );
    }

    #[test]
    fn matches_python_ecdsa_vectors() {
        let k1 = KeyPair::from_secret_bytes(&hex::decode(SK1).unwrap());
        let k2 = KeyPair::from_secret_bytes(&hex::decode(SK2).unwrap());
        assert_eq!(hex::encode(k1.public_bytes()), PK1);
        assert_eq!(hex::encode(k2.public_bytes()), PK2);
        let s12 = k1.shared_secret(&k2.public_bytes()).unwrap();
        let s21 = k2.shared_secret(&k1.public_bytes()).unwrap();
        assert_eq!(hex::encode(*s12), SHARED);
        assert_eq!(*s12, *s21);
    }

    #[test]
    fn random_keys_agree() {
        let a = KeyPair::generate();
        let b = KeyPair::generate();
        assert_eq!(
            *a.shared_secret(&b.public_bytes()).unwrap(),
            *b.shared_secret(&a.public_bytes()).unwrap()
        );
    }

    #[test]
    fn rejects_bad_peer_keys() {
        let k = KeyPair::generate();
        assert_eq!(k.shared_secret(&[0u8; 10]).unwrap_err(), EcdhError::BadLength(10));
        assert_eq!(k.shared_secret(&[1u8; 40]).unwrap_err(), EcdhError::NotOnCurve);
    }
}
