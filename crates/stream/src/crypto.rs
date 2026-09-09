//! The `Plt*` crypto functions moonlight-common-c calls (see its
//! `PlatformCrypto.h`), implemented here so no OpenSSL or mbedTLS is needed.
//!
//! GCM calls are one message each (12- or 16-byte IV, 16-byte tag, no AAD).
//! CBC is only used for pre-Gen-7 hosts (input) and encrypted audio: a
//! streaming no-padding encryptor, and a one-shot PKCS#7 decryptor.

// The C ABI fixes these signatures.
#![allow(clippy::too_many_arguments)]

use aes::cipher::{
    block_padding::{NoPadding, Pkcs7},
    generic_array::GenericArray,
    typenum::{U12, U16},
    BlockDecryptMut, BlockEncryptMut, KeyInit, KeyIvInit,
};
use aes::Aes128;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::AesGcm;
use std::os::raw::{c_int, c_void};

pub const ALGORITHM_AES_CBC: c_int = 1;
pub const ALGORITHM_AES_GCM: c_int = 2;
pub const CIPHER_FLAG_RESET_IV: c_int = 0x01;
pub const CIPHER_FLAG_FINISH: c_int = 0x02;
pub const CIPHER_FLAG_PAD_TO_BLOCK_SIZE: c_int = 0x04;

/// Opaque to C. Only the streaming CBC encryptor carries state between calls.
#[derive(Default)]
pub struct Context {
    cbc_enc: Option<cbc::Encryptor<Aes128>>,
}

#[no_mangle]
pub extern "C" fn PltCreateCryptoContext() -> *mut c_void {
    Box::into_raw(Box::new(Context::default())) as *mut c_void
}

/// # Safety
/// `ctx` must come from [`PltCreateCryptoContext`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn PltDestroyCryptoContext(ctx: *mut c_void) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx as *mut Context));
    }
}

/// # Safety
/// `data` must point to `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn PltGenerateRandomData(data: *mut u8, len: c_int) {
    if data.is_null() || len <= 0 {
        return;
    }
    let out = std::slice::from_raw_parts_mut(data, len as usize);
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), out);
}

struct Args<'a> {
    key: &'a [u8],
    iv: &'a [u8],
    tag: Option<&'a mut [u8]>,
    input: &'a [u8],
    output: &'a mut [u8],
}

/// Build safe slices from the raw pointer arguments. `out_cap` is how much of
/// `output` the caller guarantees; moonlight sizes it to the input length,
/// rounded up to a block for CBC.
unsafe fn args<'a>(
    key: *const u8,
    key_len: c_int,
    iv: *const u8,
    iv_len: c_int,
    tag: *mut u8,
    tag_len: c_int,
    input: *const u8,
    input_len: c_int,
    output: *mut u8,
    out_cap: usize,
) -> Option<Args<'a>> {
    if key.is_null() || iv.is_null() || input.is_null() || output.is_null() {
        return None;
    }
    if key_len != 16 || iv_len < 0 || input_len < 0 || tag_len < 0 {
        return None;
    }
    Some(Args {
        key: std::slice::from_raw_parts(key, 16),
        iv: std::slice::from_raw_parts(iv, iv_len as usize),
        tag: (!tag.is_null() && tag_len > 0)
            .then(|| std::slice::from_raw_parts_mut(tag, tag_len as usize)),
        input: std::slice::from_raw_parts(input, input_len as usize),
        output: std::slice::from_raw_parts_mut(output, out_cap),
    })
}

fn padded_len(n: usize) -> usize {
    n.div_ceil(16) * 16
}

fn gcm(a: &mut Args<'_>, encrypt: bool) -> Option<usize> {
    let tag = a.tag.as_deref_mut()?;
    if tag.len() != 16 {
        return None;
    }
    let out = &mut a.output[..a.input.len()];
    out.copy_from_slice(a.input);
    let key = GenericArray::from_slice(a.key);
    match (a.iv.len(), encrypt) {
        (12, true) => {
            let t = AesGcm::<Aes128, U12>::new(key)
                .encrypt_in_place_detached(GenericArray::from_slice(a.iv), &[], out)
                .ok()?;
            tag.copy_from_slice(&t);
        }
        (12, false) => AesGcm::<Aes128, U12>::new(key)
            .decrypt_in_place_detached(
                GenericArray::from_slice(a.iv),
                &[],
                out,
                GenericArray::from_slice(tag),
            )
            .ok()?,
        (16, true) => {
            let t = AesGcm::<Aes128, U16>::new(key)
                .encrypt_in_place_detached(GenericArray::from_slice(a.iv), &[], out)
                .ok()?;
            tag.copy_from_slice(&t);
        }
        (16, false) => AesGcm::<Aes128, U16>::new(key)
            .decrypt_in_place_detached(
                GenericArray::from_slice(a.iv),
                &[],
                out,
                GenericArray::from_slice(tag),
            )
            .ok()?,
        _ => return None,
    }
    Some(a.input.len())
}

/// # Safety
/// Pointer arguments follow the contract in moonlight-common-c's
/// `PlatformCrypto.h`; `output` holds at least the padded input length.
#[no_mangle]
pub unsafe extern "C" fn PltEncryptMessage(
    ctx: *mut c_void,
    algorithm: c_int,
    flags: c_int,
    key: *const u8,
    key_len: c_int,
    iv: *const u8,
    iv_len: c_int,
    tag: *mut u8,
    tag_len: c_int,
    input: *mut u8,
    input_len: c_int,
    output: *mut u8,
    output_len: *mut c_int,
) -> bool {
    if ctx.is_null() || output_len.is_null() {
        return false;
    }
    let ctx = &mut *(ctx as *mut Context);
    let mut in_len = input_len.max(0) as usize;
    if algorithm == ALGORITHM_AES_CBC && flags & CIPHER_FLAG_PAD_TO_BLOCK_SIZE != 0 {
        // The caller's buffer is sized for this and may be modified.
        let padded = padded_len(in_len);
        let pad = (padded - in_len) as u8;
        let buf = std::slice::from_raw_parts_mut(input, padded);
        for b in &mut buf[in_len..] {
            *b = if pad == 0 { 16 } else { pad };
        }
        in_len = padded;
    }
    let Some(mut a) = args(
        key,
        key_len,
        iv,
        iv_len,
        tag,
        tag_len,
        input,
        in_len as c_int,
        output,
        if algorithm == ALGORITHM_AES_CBC {
            padded_len(in_len)
        } else {
            in_len
        },
    ) else {
        return false;
    };
    let n = match algorithm {
        ALGORITHM_AES_GCM => gcm(&mut a, true),
        ALGORITHM_AES_CBC => {
            if a.iv.len() != 16 || !in_len.is_multiple_of(16) {
                None
            } else {
                if ctx.cbc_enc.is_none() || flags & CIPHER_FLAG_RESET_IV != 0 {
                    ctx.cbc_enc = Some(cbc::Encryptor::<Aes128>::new(
                        GenericArray::from_slice(a.key),
                        GenericArray::from_slice(a.iv),
                    ));
                }
                let enc = ctx.cbc_enc.as_mut().unwrap();
                let out = &mut a.output[..in_len];
                out.copy_from_slice(a.input);
                for block in out.as_chunks_mut::<16>().0 {
                    enc.encrypt_block_mut(GenericArray::from_mut_slice(block));
                }
                if flags & CIPHER_FLAG_FINISH != 0 {
                    ctx.cbc_enc = None;
                }
                Some(in_len)
            }
        }
        _ => None,
    };
    match n {
        Some(n) => {
            *output_len = n as c_int;
            true
        }
        None => false,
    }
}

/// # Safety
/// See [`PltEncryptMessage`].
#[no_mangle]
pub unsafe extern "C" fn PltDecryptMessage(
    ctx: *mut c_void,
    algorithm: c_int,
    flags: c_int,
    key: *const u8,
    key_len: c_int,
    iv: *const u8,
    iv_len: c_int,
    tag: *mut u8,
    tag_len: c_int,
    input: *mut u8,
    input_len: c_int,
    output: *mut u8,
    output_len: *mut c_int,
) -> bool {
    if ctx.is_null() || output_len.is_null() {
        return false;
    }
    let in_len = input_len.max(0) as usize;
    let Some(mut a) = args(
        key,
        key_len,
        iv,
        iv_len,
        tag,
        tag_len,
        input,
        input_len,
        output,
        if algorithm == ALGORITHM_AES_CBC {
            padded_len(in_len)
        } else {
            in_len
        },
    ) else {
        return false;
    };
    let n = match algorithm {
        ALGORITHM_AES_GCM => gcm(&mut a, false),
        ALGORITHM_AES_CBC => {
            if a.iv.len() != 16 || !in_len.is_multiple_of(16) {
                None
            } else {
                let dec = cbc::Decryptor::<Aes128>::new(
                    GenericArray::from_slice(a.key),
                    GenericArray::from_slice(a.iv),
                );
                let out = &mut a.output[..in_len];
                if flags & CIPHER_FLAG_FINISH != 0 {
                    dec.decrypt_padded_b2b_mut::<Pkcs7>(a.input, out)
                        .ok()
                        .map(|p| p.len())
                } else {
                    dec.decrypt_padded_b2b_mut::<NoPadding>(a.input, out)
                        .ok()
                        .map(|p| p.len())
                }
            }
        }
        _ => None,
    };
    match n {
        Some(n) => {
            *output_len = n as c_int;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn enc(
        ctx: *mut c_void,
        alg: c_int,
        flags: c_int,
        key: &[u8],
        iv: &[u8],
        tag: &mut [u8],
        input: &mut [u8],
        input_len: usize,
        out: &mut [u8],
    ) -> Option<usize> {
        let mut n = 0;
        PltEncryptMessage(
            ctx,
            alg,
            flags,
            key.as_ptr(),
            key.len() as c_int,
            iv.as_ptr(),
            iv.len() as c_int,
            if tag.is_empty() {
                std::ptr::null_mut()
            } else {
                tag.as_mut_ptr()
            },
            tag.len() as c_int,
            input.as_mut_ptr(),
            input_len as c_int,
            out.as_mut_ptr(),
            &mut n,
        )
        .then_some(n as usize)
    }

    unsafe fn dec(
        ctx: *mut c_void,
        alg: c_int,
        flags: c_int,
        key: &[u8],
        iv: &[u8],
        tag: &mut [u8],
        input: &mut [u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let mut n = 0;
        PltDecryptMessage(
            ctx,
            alg,
            flags,
            key.as_ptr(),
            key.len() as c_int,
            iv.as_ptr(),
            iv.len() as c_int,
            if tag.is_empty() {
                std::ptr::null_mut()
            } else {
                tag.as_mut_ptr()
            },
            tag.len() as c_int,
            input.as_mut_ptr(),
            input.len() as c_int,
            out.as_mut_ptr(),
            &mut n,
        )
        .then_some(n as usize)
    }

    #[test]
    fn gcm_round_trips_and_detects_tampering() {
        let key = [7u8; 16];
        for iv_len in [12usize, 16] {
            let iv = vec![3u8; iv_len];
            let mut msg = b"hello moonlight, this is a control message".to_vec();
            let n = msg.len();
            let mut ct = vec![0u8; n];
            let mut tag = [0u8; 16];
            let mut pt = vec![0u8; n];
            unsafe {
                let e = PltCreateCryptoContext();
                let d = PltCreateCryptoContext();
                assert_eq!(
                    enc(
                        e,
                        ALGORITHM_AES_GCM,
                        0,
                        &key,
                        &iv,
                        &mut tag,
                        &mut msg,
                        n,
                        &mut ct
                    ),
                    Some(n)
                );
                assert_ne!(ct, msg);
                assert_eq!(
                    dec(
                        d,
                        ALGORITHM_AES_GCM,
                        0,
                        &key,
                        &iv,
                        &mut tag,
                        &mut ct.clone(),
                        &mut pt
                    ),
                    Some(n)
                );
                assert_eq!(pt, msg);
                ct[0] ^= 1;
                assert_eq!(
                    dec(
                        d,
                        ALGORITHM_AES_GCM,
                        0,
                        &key,
                        &iv,
                        &mut tag,
                        &mut ct,
                        &mut pt
                    ),
                    None
                );
                PltDestroyCryptoContext(e);
                PltDestroyCryptoContext(d);
            }
        }
    }

    #[test]
    fn cbc_padded_encrypt_matches_pkcs7_decrypt() {
        let key = [1u8; 16];
        let iv = [2u8; 16];
        let text = b"twenty bytes of data";
        let mut input = vec![0u8; 32];
        input[..20].copy_from_slice(text);
        let mut ct = vec![0u8; 32];
        let mut pt = vec![0u8; 32];
        unsafe {
            let e = PltCreateCryptoContext();
            let d = PltCreateCryptoContext();
            let n = enc(
                e,
                ALGORITHM_AES_CBC,
                CIPHER_FLAG_PAD_TO_BLOCK_SIZE | CIPHER_FLAG_RESET_IV,
                &key,
                &iv,
                &mut [],
                &mut input,
                20,
                &mut ct,
            );
            assert_eq!(n, Some(32));
            let m = dec(
                d,
                ALGORITHM_AES_CBC,
                CIPHER_FLAG_RESET_IV | CIPHER_FLAG_FINISH,
                &key,
                &iv,
                &mut [],
                &mut ct,
                &mut pt,
            );
            assert_eq!(m, Some(20));
            assert_eq!(&pt[..20], text);
            PltDestroyCryptoContext(e);
            PltDestroyCryptoContext(d);
        }
    }

    #[test]
    fn random_fills_the_buffer() {
        let mut a = [0u8; 32];
        unsafe { PltGenerateRandomData(a.as_mut_ptr(), 32) };
        assert!(a.iter().any(|&b| b != 0));
    }
}
