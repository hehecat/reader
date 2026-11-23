//! 加密 / 摘要 / 编码 / 压缩桥接：
//! - `xGorgon` / `gzip`
//! - `java.md5Encode` / `md5Encode16` / `digestHex` / `digestBase64Str` / `HMacHex`
//! - `java.base64Encode` / `base64Decode` / `base64DecodeToString` / `base64DecodeToByteArray`
//! - `java.hexDecodeToString` / `escape` / `unescape` / `utf8ToGbk`
//! - `java.createSymmetricCrypto` + AES / DES / 3DES 全系列加解密方法

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use boa_engine::object::builtins::JsArray;
use boa_engine::object::ObjectInitializer;
use boa_engine::{Context, JsArgs, JsResult, JsString, JsValue, NativeFunction};

use super::runtime::{
    js_byte_array, js_bytes_to_js_array, js_bytes_to_js_string, js_native_error, js_value_to_bytes,
    js_value_to_string, JsBridgeInner,
};

/// xGorgon(...)：字节系请求签名 stub（无法复现——返回空串）
pub(super) fn xgorgon_stub(
    _this: &JsValue,
    _args: &[JsValue],
    _context: &mut Context,
) -> JsResult<JsValue> {
    Ok(JsValue::from(JsString::from("")))
}

/// 全局 gzip(text)：UTF-8 → GZip → base64（空输入返回空串）
pub(super) fn gzip_base64(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    use std::io::Write as _;
    let text = js_value_to_string(args.get_or_undefined(0), context);
    if text.is_empty() {
        return Ok(JsValue::from(JsString::from("")));
    }
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let out = enc
        .write_all(text.as_bytes())
        .and_then(|_| enc.finish())
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(&bytes))
        .unwrap_or_default();
    Ok(JsValue::from(JsString::from(out)))
}

// ---- 签名 / 摘要 / 编码 shim ----

/// java.md5Encode(str)：md5 十六进制（小写）
pub(super) fn java_md5_encode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(crate::util::md5::md5_encode(
        &s,
    ))))
}

/// java.HMacHex(data, algo, key)：HMAC 十六进制（HmacMD5/HmacSHA1/HmacSHA256/HmacSHA512）
pub(super) fn java_hmac_hex(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context);
    let key = js_value_to_string(args.get_or_undefined(2), context);
    use hmac::{Mac, SimpleHmac};
    let out: String = match algo.to_lowercase().as_str() {
        "hmacmd5" => hex::encode(
            SimpleHmac::<md5::Md5>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha1" => hex::encode(
            SimpleHmac::<sha1::Sha1>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha256" => hex::encode(
            SimpleHmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        "hmacsha512" => hex::encode(
            SimpleHmac::<sha2::Sha512>::new_from_slice(key.as_bytes())
                .map_err(|_| js_native_error("HMacHex: 密钥长度非法"))?
                .chain_update(data.as_bytes())
                .finalize()
                .into_bytes(),
        ),
        other => return Err(js_native_error(format!("HMacHex: 不支持的算法 {other}"))),
    };
    Ok(JsValue::from(JsString::from(out)))
}

/// E16：Android Base64 flags → base64 引擎
/// （NO_PADDING=1 / NO_WRAP=2 / CRLF=4 / URL_SAFE=8；解码恒宽松——容忍换行/缺省填充）
fn b64_engine(flags: i64) -> base64::engine::GeneralPurpose {
    use base64::alphabet;
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let alpha = if flags & 8 != 0 {
        alphabet::URL_SAFE
    } else {
        alphabet::STANDARD
    };
    let cfg = GeneralPurposeConfig::new()
        .with_encode_padding(flags & 1 == 0)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&alpha, cfg)
}

/// 手动 76 列折行（Android DEFAULT/CRLF 换行形态；NO_WRAP 不折）
fn b64_wrap(s: String, flags: i64) -> String {
    if flags & 2 != 0 || s.len() <= 76 {
        return s;
    }
    let eol = if flags & 4 != 0 { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(s.len() + s.len() / 76 * eol.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = (i + 76).min(bytes.len());
        if i > 0 {
            out.push_str(eol);
        }
        out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or_default());
        i = end;
    }
    out
}

/// 解码前剥离空白（Android DEFAULT 输出带 \n——解码侧恒容忍）
fn b64_strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// java.base64Encode(str[, flags])
pub(super) fn java_base64_encode_flags(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    let encoded = b64_engine(flags).encode(s.as_bytes());
    Ok(JsValue::from(JsString::from(b64_wrap(encoded, flags))))
}

/// java.base64DecodeToString(str[, flags]) → 文本（UTF-8 lossy）
pub(super) fn java_base64_decode_flags(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    match b64_engine(flags).decode(b64_strip_ws(&s).as_bytes()) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(_) => Err(js_native_error("java.base64Decode: base64 解码失败")),
    }
}

/// java.base64DecodeToByteArray(str[, flags]) → number[]（每元素 0-255）
pub(super) fn java_base64_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    let flags = args
        .get(1)
        .and_then(|v| v.as_number())
        .map(|n| n as i64)
        .unwrap_or(0);
    match b64_engine(flags).decode(b64_strip_ws(&s).as_bytes()) {
        Ok(bytes) => {
            let arr = JsArray::from_iter(bytes.iter().map(|b| JsValue::from(*b as i32)), context);
            Ok(arr.into())
        }
        Err(_) => Err(js_native_error(
            "java.base64DecodeToByteArray: base64 解码失败",
        )),
    }
}

/// java.digestBase64Str(data, algorithm)：摘要 → base64（digestHex 的 base64 版）
pub(super) fn java_digest_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    use sha1::Digest as _;
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context).to_ascii_lowercase();
    let digest: Vec<u8> = match algo.as_str() {
        "md5" | "md-5" => hex::decode(crate::util::md5::md5_encode(&data)).unwrap_or_default(),
        "sha1" | "sha-1" => sha1::Sha1::digest(data.as_bytes()).to_vec(),
        "sha256" | "sha-256" => sha2::Sha256::digest(data.as_bytes()).to_vec(),
        "sha512" | "sha-512" => sha2::Sha512::digest(data.as_bytes()).to_vec(),
        other => {
            return Err(js_native_error(format!(
                "digestBase64Str: 不支持的算法 {other}"
            )))
        }
    };
    Ok(JsValue::from(JsString::from(
        base64::engine::general_purpose::STANDARD.encode(digest),
    )))
}

/// java.hexDecodeToString(str)：十六进制 → 文本
pub(super) fn java_hex_decode(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    match hex::decode(s.trim()) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            String::from_utf8_lossy(&bytes).into_owned(),
        ))),
        Err(_) => Err(js_native_error("java.hexDecodeToString: 十六进制解码失败")),
    }
}

/// Java `escape`：ASCII 字母数字保留，其他 <256 用 %XX，>=256 用 %uXXXX
fn java_escape_impl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let code = c as u32;
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if code < 0x100 {
            out.push_str(&format!("%{code:02X}"));
        } else {
            out.push_str(&format!("%u{code:04X}"));
        }
    }
    out
}

/// Java `unescape`：%XX / %uXXXX
fn java_unescape_impl(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && bytes[i + 1] == b'u' {
            let hex = s.get(i + 2..i + 6).unwrap_or("");
            if let Ok(code) = u32::from_str_radix(hex, 16) {
                if let Some(c) = char::from_u32(code) {
                    out.push(c);
                    i += 6;
                    continue;
                }
            }
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = s.get(i + 1..i + 3).unwrap_or("");
            if let Ok(code) = u8::from_str_radix(hex, 16) {
                out.push(code as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// legacy utf8ToGbk：UTF-8 字符串 → GBK 字节 → 按 UTF-8 解码（书源签名场景）
fn utf8_to_gbk_lossy(s: &str) -> String {
    let (gbk_bytes, _, _) = encoding_rs::GBK.encode(s);
    String::from_utf8_lossy(&gbk_bytes).into_owned()
}

pub(super) fn java_escape(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(java_escape_impl(&s))))
}

pub(super) fn java_unescape(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(java_unescape_impl(&s))))
}

pub(super) fn java_utf8_to_gbk(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(utf8_to_gbk_lossy(&s))))
}

pub(super) fn java_digest_hex(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_string(args.get_or_undefined(0), context);
    let algo = js_value_to_string(args.get_or_undefined(1), context).to_ascii_lowercase();
    use sha1::Digest as _;
    let out = match algo.as_str() {
        "md5" | "md-5" => crate::util::md5::md5_encode(&data),
        "sha1" | "sha-1" => hex::encode(sha1::Sha1::digest(data.as_bytes())),
        "sha256" | "sha-256" => hex::encode(sha2::Sha256::digest(data.as_bytes())),
        "sha512" | "sha-512" => hex::encode(sha2::Sha512::digest(data.as_bytes())),
        other => return Err(js_native_error(format!("digestHex: 不支持的算法 {other}"))),
    };
    Ok(JsValue::from(JsString::from(out)))
}

pub(super) fn java_md5_encode16(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let s = js_value_to_string(args.get_or_undefined(0), context);
    Ok(JsValue::from(JsString::from(
        crate::util::md5::md5_encode(&s)[8..24].to_string(),
    )))
}

// ---- 对称加密底层（AES / DES / 3DES） ----

/// Java transformation 字符串解析（AES/DES/DESede + ECB/CBC + PKCS5/7/NoPadding）
fn parse_transformation(transformation: &str) -> Result<(String, String, String), String> {
    let parts: Vec<&str> = transformation.split('/').collect();
    if parts.len() != 3 {
        return Err(format!("不支持的 transformation: {transformation}"));
    }
    let algo = parts[0].to_ascii_uppercase();
    let mode = parts[1].to_ascii_uppercase();
    let pad = parts[2].to_ascii_uppercase();
    let algo_ok = matches!(
        algo.as_str(),
        "AES" | "DES" | "DESEDE" | "TRIPLEDES" | "3DES"
    );
    let mode_ok = matches!(mode.as_str(), "ECB" | "CBC");
    let pad_ok = matches!(
        pad.as_str(),
        "PKCS5PADDING" | "PKCS7PADDING" | "PKCS5" | "PKCS7" | "NOPADDING" | "NO"
    );
    if !algo_ok || !mode_ok || !pad_ok {
        return Err(format!("不支持的 transformation: {transformation}"));
    }
    Ok((algo, mode, pad))
}

fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad = block_size - (data.len() % block_size);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(pad as u8, pad));
    out
}

fn pkcs7_unpad(data: &[u8]) -> Option<Vec<u8>> {
    let last = *data.last()? as usize;
    if last == 0 || last > data.len() {
        return None;
    }
    if data[data.len() - last..]
        .iter()
        .all(|&b| b as usize == last)
    {
        Some(data[..data.len() - last].to_vec())
    } else {
        None
    }
}

/// 手动 CBC（兼容 cipher 0.4 / 0.5 两代 block cipher trait）
fn cbc_apply(
    encrypt_block: impl Fn(&[u8]) -> Result<Vec<u8>, String>,
    decrypt_block: impl Fn(&[u8]) -> Result<Vec<u8>, String>,
    data: &[u8],
    iv: &[u8],
    block_size: usize,
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    if data.is_empty() || !data.len().is_multiple_of(block_size) {
        return Err("CBC 数据长度必须为分块大小的整数倍".into());
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = iv.to_vec();
    for chunk in data.chunks(block_size) {
        if encrypt {
            let xored: Vec<u8> = chunk.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
            let enc = encrypt_block(&xored)?;
            out.extend_from_slice(&enc);
            prev = enc;
        } else {
            let dec = decrypt_block(chunk)?;
            let plain: Vec<u8> = dec.iter().zip(prev.iter()).map(|(a, b)| a ^ b).collect();
            out.extend_from_slice(&plain);
            prev = chunk.to_vec();
        }
    }
    Ok(out)
}

/// AES 加解密（ECB/CBC + PKCS5/7/NoPadding；key 16/24/32，iv 前 16 字节）
pub(super) fn aes_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
    let (_, mode, pad) = parse_transformation(transformation)?;
    let padding = !matches!(pad.as_str(), "NOPADDING" | "NO");
    let key_len = match key.len() {
        16 | 24 | 32 => key.len(),
        _ => return Err(format!("AES key 长度必须为 16/24/32，实际 {}", key.len())),
    };
    let iv_bytes = if iv.is_empty() {
        vec![0u8; 16]
    } else {
        iv.iter().take(16).copied().collect::<Vec<u8>>()
    };
    let data = if encrypt && padding {
        pkcs7_pad(data, 16)
    } else {
        data.to_vec()
    };
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err("AES 数据长度不合法".into());
    }
    let block_enc = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(16);
        match key_len {
            16 => {
                let c = aes::Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes128>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            24 => {
                let c = aes::Aes192::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes192>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            32 => {
                let c = aes::Aes256::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes256>::clone_from_slice(block);
                c.encrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            _ => return Err("AES key 长度不合法".into()),
        }
        Ok(out)
    };
    let block_dec = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(16);
        match key_len {
            16 => {
                let c = aes::Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes128>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            24 => {
                let c = aes::Aes192::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes192>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            32 => {
                let c = aes::Aes256::new_from_slice(key).map_err(|e| e.to_string())?;
                let mut b = aes::cipher::Block::<aes::Aes256>::clone_from_slice(block);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            _ => return Err("AES key 长度不合法".into()),
        }
        Ok(out)
    };
    let raw = if mode == "CBC" {
        cbc_apply(block_enc, block_dec, &data, &iv_bytes, 16, encrypt)?
    } else {
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let r = if encrypt {
                block_enc(chunk)?
            } else {
                block_dec(chunk)?
            };
            out.extend_from_slice(&r);
        }
        out
    };
    if encrypt || !padding {
        Ok(raw)
    } else {
        pkcs7_unpad(&raw).ok_or_else(|| "AES 解密填充校验失败".into())
    }
}

/// DES/DESede 加解密（ECB/CBC + PKCS5/7/NoPadding；cipher 0.5 block trait）
fn des_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    use des::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
    let (algo, mode, pad) = parse_transformation(transformation)?;
    let padding = !matches!(pad.as_str(), "NOPADDING" | "NO");
    let is_3des = matches!(algo.as_str(), "DESEDE" | "TRIPLEDES" | "3DES");
    let key_len = if is_3des { 24 } else { 8 };
    if key.len() != key_len {
        return Err(format!(
            "{} key 长度必须为 {key_len}，实际 {}",
            if is_3des { "3DES" } else { "DES" },
            key.len()
        ));
    }
    let iv_bytes = if iv.is_empty() {
        vec![0u8; 8]
    } else {
        iv.iter().take(8).copied().collect::<Vec<u8>>()
    };
    let data = if encrypt && padding {
        pkcs7_pad(data, 8)
    } else {
        data.to_vec()
    };
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return Err("DES 数据长度不合法".into());
    }
    let block_enc = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(8);
        if is_3des {
            let c = des::TdesEde3::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::TdesEde3>::clone_from_slice(block);
            c.encrypt_block(&mut b);
            out.extend_from_slice(&b);
        } else {
            let c = des::Des::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::Des>::clone_from_slice(block);
            c.encrypt_block(&mut b);
            out.extend_from_slice(&b);
        }
        Ok(out)
    };
    let block_dec = |block: &[u8]| -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(8);
        if is_3des {
            let c = des::TdesEde3::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::TdesEde3>::clone_from_slice(block);
            c.decrypt_block(&mut b);
            out.extend_from_slice(&b);
        } else {
            let c = des::Des::new_from_slice(key).map_err(|e| e.to_string())?;
            let mut b = des::cipher::Block::<des::Des>::clone_from_slice(block);
            c.decrypt_block(&mut b);
            out.extend_from_slice(&b);
        }
        Ok(out)
    };
    let raw = if mode == "CBC" {
        cbc_apply(block_enc, block_dec, &data, &iv_bytes, 8, encrypt)?
    } else {
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(8) {
            let r = if encrypt {
                block_enc(chunk)?
            } else {
                block_dec(chunk)?
            };
            out.extend_from_slice(&r);
        }
        out
    };
    if encrypt || !padding {
        Ok(raw)
    } else {
        pkcs7_unpad(&raw).ok_or_else(|| "DES 解密填充校验失败".into())
    }
}

/// 通用对称加解密分发（AES/DES/DESede）
fn symmetric_crypt(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let (algo, _, _) = parse_transformation(transformation)?;
    match algo.as_str() {
        "AES" => aes_crypt(data, key, transformation, iv, encrypt),
        _ => des_crypt(data, key, transformation, iv, encrypt),
    }
}

/// `java.createSymmetricCrypto` 返回的 cipher 对象状态（`setIv` 可改写 iv）
struct SymmetricCryptoState {
    transformation: String,
    key: Vec<u8>,
    iv: Vec<u8>,
}

/// 对称密钥长度归一化：AES 16/24/32、DES 8、3DES 24；
/// 短键补零，16 字节 3DES 键按 TDEA-2 复制前 8 字节
fn normalize_symmetric_key(algo: &str, key: &[u8]) -> Vec<u8> {
    let len = match algo {
        "AES" => {
            if key.len() >= 32 {
                32
            } else if key.len() >= 24 {
                24
            } else {
                16
            }
        }
        "DES" => 8,
        _ => 24,
    };
    let mut out = vec![0u8; len];
    let n = key.len().min(len);
    out[..n].copy_from_slice(&key[..n]);
    if algo != "AES" && algo != "DES" && key.len() == 16 {
        out[16..24].copy_from_slice(&key[..8]);
    }
    out
}

fn normalize_symmetric_iv(algo: &str, iv: &[u8]) -> Vec<u8> {
    let block = if algo == "AES" { 16 } else { 8 };
    let mut out = vec![0u8; block];
    let n = iv.len().min(block);
    out[..n].copy_from_slice(&iv[..n]);
    out
}

/// legado `decrypt(data)` 的 data 编码识别：
/// ByteArray 原样，字符串按 `isHex`（偶数位全 hex）→ hex，否则 base64
fn symmetric_data_to_bytes(v: &JsValue, context: &mut Context) -> Vec<u8> {
    if v.as_object().is_some() {
        return js_value_to_bytes(v, context);
    }
    let s = js_value_to_string(v, context);
    let s = s.trim();
    let is_hex =
        !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex {
        return hex::decode(s).unwrap_or_default();
    }
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .unwrap_or_default()
}

fn symmetric_cipher_do(
    state: &SymmetricCryptoState,
    data: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let (algo, _, _) = parse_transformation(&state.transformation)?;
    let key = normalize_symmetric_key(&algo, &state.key);
    let iv = normalize_symmetric_iv(&algo, &state.iv);
    symmetric_crypt(data, &key, &state.transformation, &iv, encrypt)
}

/// `java.createSymmetricCrypto(transformation, key, iv)`：
/// legado JsEncodeUtils 对称加密对象（decrypt/decryptStr/encrypt/encryptBase64/encryptHex/setIv）
pub(super) fn java_create_symmetric_crypto(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let transformation = js_value_to_string(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let iv = js_value_to_bytes(args.get_or_undefined(2), context);
    let state = Arc::new(Mutex::new(SymmetricCryptoState {
        transformation,
        key,
        iv,
    }));

    let decrypt_state = Arc::clone(&state);
    let decrypt = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = decrypt_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = symmetric_data_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, false)
                .map(|bytes| js_byte_array(bytes, ctx))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.decrypt: {e}")))
        })
    };

    let decrypt_str_state = Arc::clone(&state);
    let decrypt_str = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = decrypt_str_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = symmetric_data_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, false)
                .map(js_bytes_to_js_string)
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.decryptStr: {e}")))
        })
    };

    let encrypt_state = Arc::clone(&state);
    let encrypt = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| js_byte_array(bytes, ctx))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encrypt: {e}")))
        })
    };

    let encrypt_b64_state = Arc::clone(&state);
    let encrypt_b64 = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_b64_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| {
                    JsValue::from(JsString::from(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ))
                })
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encryptBase64: {e}")))
        })
    };

    let encrypt_hex_state = Arc::clone(&state);
    let encrypt_hex = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let st = encrypt_hex_state.lock().unwrap_or_else(|e| e.into_inner());
            let data = js_value_to_bytes(args.get_or_undefined(0), ctx);
            symmetric_cipher_do(&st, &data, true)
                .map(|bytes| JsValue::from(JsString::from(hex::encode(bytes))))
                .map_err(|e| js_native_error(format!("createSymmetricCrypto.encryptHex: {e}")))
        })
    };

    let set_iv_state = Arc::clone(&state);
    let set_iv = unsafe {
        NativeFunction::from_closure(move |this, args, ctx| {
            let iv = js_value_to_bytes(args.get_or_undefined(0), ctx);
            set_iv_state.lock().unwrap_or_else(|e| e.into_inner()).iv = iv;
            Ok(this.clone())
        })
    };

    let obj = ObjectInitializer::new(context)
        .function(decrypt, JsString::from("decrypt"), 1)
        .function(decrypt_str, JsString::from("decryptStr"), 1)
        .function(encrypt, JsString::from("encrypt"), 1)
        .function(encrypt_b64, JsString::from("encryptBase64"), 1)
        .function(encrypt_hex, JsString::from("encryptHex"), 1)
        .function(set_iv, JsString::from("setIv"), 1)
        .build();
    Ok(obj.into())
}

/// 通用对称加解密（transformation 四参数格式）
fn crypt_with_transformation(
    data: &[u8],
    key: &[u8],
    transformation: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    symmetric_crypt(data, key, transformation, iv, encrypt)
}

/// mode/padding 分开的 Hutool 风格调用（AES/3DES）
fn crypt_with_mode_padding(
    data: &[u8],
    key: &[u8],
    algo: &str,
    mode: &str,
    padding: &str,
    iv: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let transformation = format!("{algo}/{mode}/{padding}");
    symmetric_crypt(data, key, &transformation, iv, encrypt)
}

fn js_base64_opt(args: &[JsValue], idx: usize, context: &mut Context) -> Vec<u8> {
    let s = js_value_to_string(args.get_or_undefined(idx), context);
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .unwrap_or_default()
}

// ---- AES 系列（legacy JsExtensions 同名） ----

pub(super) fn java_aes_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeToString: {e}"))),
    }
}

pub(super) fn java_aes_base64_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.aesBase64DecodeToString: {e}"
        ))),
    }
}

pub(super) fn java_aes_encode_to_base64_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.aesEncodeToBase64String: {e}"
        ))),
    }
}

pub(super) fn java_aes_encode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesEncodeToString: {e}"))),
    }
}

pub(super) fn java_aes_decode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "AES", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeArgsBase64Str: {e}"))),
    }
}

pub(super) fn java_aes_encode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "AES", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!("java.aesEncodeArgsBase64Str: {e}"))),
    }
}

// ---- AES 系列 *ToByteArray 变体（legacy JsExtensions 同名；输出 number[]） ----

/// java.aesDecodeToByteArray(data, key, transformation, iv) → number[]
pub(super) fn java_aes_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!("java.aesDecodeToByteArray: {e}"))),
    }
}

/// java.aesBase64DecodeToByteArray(data, key, transformation, iv)：先 base64 解码再解密 → number[]
pub(super) fn java_aes_base64_decode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!(
            "java.aesBase64DecodeToByteArray: {e}"
        ))),
    }
}

/// java.aesEncodeToByteArray(data, key, transformation, iv) → number[]
pub(super) fn java_aes_encode_to_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_array(bytes, context)),
        Err(e) => Err(js_native_error(format!("java.aesEncodeToByteArray: {e}"))),
    }
}

/// java.aesEncodeToBase64ByteArray(data, key, transformation, iv)：加密后 base64 化，
/// 返回 base64 文本的字节（legacy encryptAES2Base64——aesEncodeToBase64String 即其 UTF-8 视图）
pub(super) fn java_aes_encode_to_base64_byte_array(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(js_bytes_to_js_array(b64.into_bytes(), context))
        }
        Err(e) => Err(js_native_error(format!(
            "java.aesEncodeToBase64ByteArray: {e}"
        ))),
    }
}

// ---- DES 系列 ----

pub(super) fn java_des_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.desDecodeToString: {e}"))),
    }
}

pub(super) fn java_des_base64_decode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.desBase64DecodeToString: {e}"
        ))),
    }
}

pub(super) fn java_des_encode_to_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.desEncodeToString: {e}"))),
    }
}

pub(super) fn java_des_encode_to_base64_string(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let transformation = js_value_to_string(args.get_or_undefined(2), context);
    let iv = js_value_to_bytes(args.get_or_undefined(3), context);
    match crypt_with_transformation(&data, &key, &transformation, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.desEncodeToBase64String: {e}"
        ))),
    }
}

// ---- 3DES 系列（Hutool mode/padding 风格） ----

pub(super) fn java_triple_des_decode_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_value_to_bytes(args.get_or_undefined(4), context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!("java.tripleDESDecodeStr: {e}"))),
    }
}

pub(super) fn java_triple_des_decode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_base64_opt(args, 0, context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, false) {
        Ok(bytes) => Ok(js_bytes_to_js_string(bytes)),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESDecodeArgsBase64Str: {e}"
        ))),
    }
}

pub(super) fn java_triple_des_encode_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_value_to_bytes(args.get_or_undefined(1), context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_value_to_bytes(args.get_or_undefined(4), context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESEncodeBase64Str: {e}"
        ))),
    }
}

pub(super) fn java_triple_des_encode_args_base64_str(
    _inner: &JsBridgeInner,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let data = js_value_to_bytes(args.get_or_undefined(0), context);
    let key = js_base64_opt(args, 1, context);
    let mode = js_value_to_string(args.get_or_undefined(2), context);
    let padding = js_value_to_string(args.get_or_undefined(3), context);
    let iv = js_base64_opt(args, 4, context);
    match crypt_with_mode_padding(&data, &key, "DESede", &mode, &padding, &iv, true) {
        Ok(bytes) => Ok(JsValue::from(JsString::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ))),
        Err(e) => Err(js_native_error(format!(
            "java.tripleDESEncodeArgsBase64Str: {e}"
        ))),
    }
}
