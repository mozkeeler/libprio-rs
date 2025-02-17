// SPDX-License-Identifier: MPL-2.0

//! This module implements PRGs as specified in draft-patton-cfrg-vdaf-01.

use crate::vdaf::{CodecError, Decode, Encode};
use aes::{
    cipher::{KeyIvInit, StreamCipher},
    Aes128,
};
use cmac::{Cmac, Mac};
use ctr::Ctr64BE;
use std::{
    fmt::{Debug, Formatter},
    io::{Cursor, Read},
};

/// Function pointer to fill a buffer with random bytes. Under normal operation,
/// `getrandom::getrandom()` will be used, but other implementations can be used to control
/// randomness when generating or verifying test vectors.
pub(crate) type RandSource = fn(&mut [u8]) -> Result<(), getrandom::Error>;

/// Input of [`Prg`].
#[derive(Clone, Debug, Eq)]
pub struct Seed<const L: usize>(pub(crate) [u8; L]);

impl<const L: usize> Seed<L> {
    /// Generate a uniform random seed.
    pub fn generate() -> Result<Self, getrandom::Error> {
        Self::from_rand_source(getrandom::getrandom)
    }

    pub(crate) fn from_rand_source(rand_source: RandSource) -> Result<Self, getrandom::Error> {
        let mut seed = [0; L];
        rand_source(&mut seed)?;
        Ok(Self(seed))
    }

    pub(crate) fn uninitialized() -> Self {
        Self([0; L])
    }

    pub(crate) fn xor_accumulate(&mut self, other: &Self) {
        for i in 0..L {
            self.0[i] ^= other.0[i]
        }
    }

    pub(crate) fn xor(&mut self, left: &Self, right: &Self) {
        for i in 0..L {
            self.0[i] = left.0[i] ^ right.0[i]
        }
    }
}

impl<const L: usize> PartialEq for Seed<L> {
    fn eq(&self, other: &Self) -> bool {
        // Do constant-time compare.
        let mut r = 0;
        for (x, y) in (&self.0[..]).iter().zip(&other.0[..]) {
            r |= x ^ y;
        }
        r == 0
    }
}

impl<const L: usize> Encode for Seed<L> {
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.0[..]);
    }
}

impl<const L: usize> Decode for Seed<L> {
    fn decode(bytes: &mut Cursor<&[u8]>) -> Result<Self, CodecError> {
        let mut seed = [0; L];
        bytes.read_exact(&mut seed)?;
        Ok(Seed(seed))
    }
}

/// A stream of pseudorandom bytes derived from a seed.
pub trait SeedStream {
    /// Fill `buf` with the next `buf.len()` bytes of output.
    fn fill(&mut self, buf: &mut [u8]);
}

/// A pseudorandom generator (PRG) with the interface specified in
/// [VDAF](https://datatracker.ietf.org/doc/draft-patton-cfrg-vdaf/).
pub trait Prg<const L: usize>: Clone + Debug {
    /// The type of stream produced by this PRG.
    type SeedStream: SeedStream;

    /// Construct an instance of [`Prg`] with the given seed.
    fn init(seed: &Seed<L>) -> Self;

    /// Update the PRG state by passing in the next fragment of the info string. The final info
    /// string is assembled from the concatenation of sequence of fragments passed to this method.
    fn update(&mut self, data: &[u8]);

    /// Finalize the PRG state, producing a seed stream.
    fn into_seed_stream(self) -> Self::SeedStream;

    /// Finalize the PRG state, producing a seed.
    fn into_seed(self) -> Seed<L> {
        let mut new_seed = [0; L];
        let mut seed_stream = self.into_seed_stream();
        seed_stream.fill(&mut new_seed);
        Seed(new_seed)
    }

    /// Construct a seed stream from the given seed and info string.
    fn seed_stream(seed: &Seed<L>, info: &[u8]) -> Self::SeedStream {
        let mut prg = Self::init(seed);
        prg.update(info);
        prg.into_seed_stream()
    }
}

/// The PRG based on AES128 as specifed in
/// [VDAF](https://datatracker.ietf.org/doc/draft-patton-cfrg-vdaf/).
#[derive(Clone, Debug)]
pub struct PrgAes128(Cmac<Aes128>);

impl Prg<16> for PrgAes128 {
    type SeedStream = SeedStreamAes128;

    fn init(seed: &Seed<16>) -> Self {
        Self(Cmac::new_from_slice(&seed.0).unwrap())
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    fn into_seed_stream(self) -> SeedStreamAes128 {
        let key = self.0.finalize().into_bytes();
        SeedStreamAes128::new(&key, &[0; 16])
    }
}

/// The key stream produced by AES128 in CTR-mode.
pub struct SeedStreamAes128(Ctr64BE<Aes128>);

impl SeedStreamAes128 {
    pub(crate) fn new(key: &[u8], iv: &[u8]) -> Self {
        SeedStreamAes128(Ctr64BE::<Aes128>::new(key.into(), iv.into()))
    }
}

impl SeedStream for SeedStreamAes128 {
    fn fill(&mut self, buf: &mut [u8]) {
        buf.fill(0);
        self.0.apply_keystream(buf);
    }
}

impl Debug for SeedStreamAes128 {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Ctr64BE<Aes128> does not implement Debug, but [`ctr::CtrCore`][1] does, and we get that
        // with [`cipher::StreamCipherCoreWrapper::get_core`][2].
        //
        // [1]: https://docs.rs/ctr/latest/ctr/struct.CtrCore.html
        // [2]: https://docs.rs/cipher/latest/cipher/struct.StreamCipherCoreWrapper.html
        self.0.get_core().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{field::Field128, prng::Prng};
    use serde::{Deserialize, Serialize};
    use std::convert::TryInto;

    // Test vector generated by the reference implementation. See
    // https://github.com/cjpatton/vdaf/blob/main/poc/prg.sage.
    const TEST_PRG_AES128_FIELD128: &str = r#"{
  "seed": "01010101010101010101010101010101",
  "info": "696e666f20737472696e67",
  "length": 40,
  "derived_seed": "ccf3be704c982182ad2961e9795a88aa",
  "expanded_vec": "ccf3be704c982182ad2961e9795a88aa8df71c0b5ea5c13bcf3173c3f3626505e1bf4738874d5405805082cc38c55d1f04f85fbb88b8cf8592ffed8a4ac7f76991c58d850a15e8deb34fb289ab6fab584554ffef16c683228db2b76e792ca4f3c15760044d0703b438c2aefd7975c5dd4b9992ee6f87f20e570572dea18fa580ee17204903c1234f1332d47a442ea636580518ce7aa5943c415117460a049bc19cc81edbb0114d71890cbdbe4ea2664cd038e57b88fb7fd3557830ad363c20b9840d35fd6bee6c3c8424f026ee7fbca3daf3c396a4d6736d7bd3b65b2c228d22a40f4404e47c61b26ac3c88bebf2f268fa972f8831f18bee374a22af0f8bb94d9331a1584bdf8cf3e8a5318b546efee8acd28f6cba8b21b9d52acbae8e726500340da98d643d0a5f1270ecb94c574130cea61224b0bc6d438b2f4f74152e72b37e6a9541c9dc5515f8f98fd0d1bce8743f033ab3e8574180ffc3363f3a0490f6f9583bf73a87b9bb4b51bfd0ef260637a4288c37a491c6cbdc46b6a86cd26edf611793236e912e7227bfb85b560308b06238bbd978f72ed4a58583cf0c6e134066eb6b399ad2f26fa01d69a62d8a2d04b4b8acf82299b07a834d4c2f48fee23a24c20307f9cabcd34b6d69f1969588ebde777e46e9522e866e6dd1e14119a1cb4c0709fa9ea347d9f872e76a39313e7d49bfbf3e5ce807183f43271ba2b5c6aaeaef22da301327c1fd9fedde7c5a68d9b97fa6eb687ec8ca692cb0f631f46e6699a211a1254026c9a0a43eceb450dc97cfa923321baf1f4b6f233260d46182b844dccec153aaddd20f920e9e13ff11434bcd2aa632bf4f544f41b5ddced962939676476f70e0b8640c3471fc7af62d80053781295b070388f7b7f1fa66220cb3"
}
"#;

    #[derive(Deserialize, Serialize)]
    struct PrgTestVector {
        #[serde(with = "hex")]
        seed: Vec<u8>,
        #[serde(with = "hex")]
        info: Vec<u8>,
        length: usize,
        #[serde(with = "hex")]
        derived_seed: Vec<u8>,
        #[serde(with = "hex")]
        expanded_vec: Vec<u8>,
    }

    // Test correctness of dervied methods.
    fn test_prg<P, const L: usize>()
    where
        P: Prg<L>,
    {
        let seed = Seed::generate().unwrap();
        let info = b"info string";

        let mut prg = P::init(&seed);
        prg.update(info);

        let mut want: Seed<L> = Seed::uninitialized();
        prg.clone().into_seed_stream().fill(&mut want.0[..]);
        let got = prg.clone().into_seed();
        assert_eq!(got, want);

        let mut want = [0; 45];
        prg.clone().into_seed_stream().fill(&mut want);
        let mut got = [0; 45];
        P::seed_stream(&seed, info).fill(&mut got);
        assert_eq!(got, want);
    }

    #[test]
    fn prg_aes128() {
        let t: PrgTestVector = serde_json::from_str(TEST_PRG_AES128_FIELD128).unwrap();
        let mut prg = PrgAes128::init(&Seed(t.seed.try_into().unwrap()));
        prg.update(&t.info);

        assert_eq!(
            prg.clone().into_seed(),
            Seed(t.derived_seed.try_into().unwrap())
        );

        let mut bytes = std::io::Cursor::new(t.expanded_vec.as_slice());
        let mut want = Vec::with_capacity(t.length);
        while (bytes.position() as usize) < t.expanded_vec.len() {
            want.push(Field128::decode(&mut bytes).unwrap())
        }
        let got: Vec<Field128> = Prng::from_seed_stream(prg.clone().into_seed_stream())
            .take(t.length)
            .collect();
        assert_eq!(got, want);

        test_prg::<PrgAes128, 16>();
    }
}
