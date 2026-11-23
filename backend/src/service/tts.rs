//! TTS 语音合成服务（F-25）：微软 Edge 语音（WebSocket 直连合成）+ HttpTTS（httpTTS 引擎代理）
//!
//! Edge TTS：对接 `wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1`
//! 协议（与 edge-tts 一致）：
//! 1. WSS 握手（query 携带 TrustedClientToken + Sec-MS-GEC 鉴权 token）
//! 2. 发送 speech.config（输出格式 audio-24khz-48kbitrate-mono-mp3）
//! 3. 发送 SSML 合成请求（voice + prosody rate/pitch）
//! 4. 读取二进制消息：2 字节头长 + JSON 头 + MP3 音频帧，直到 turn.end

use anyhow::{anyhow, Result};
use futures::SinkExt;
use futures::StreamExt;

/// 单个 Edge 语音（输出 JSON 兼容 legado EdgeTTS 实体）
/// A3 textToSpeechCn 中文 TTS 引擎（Pro ttsByTextToSpeechCn 对齐）：
/// POST https://www.text-to-speech.cn/getSpeek.php 表单 12 字段，
/// 响应 {download:"<音频URL>"}——调用方以 302 让客户端直连源站下载。
/// 超时 5s 对齐 Pro；站点不可达/解析失败由调用方回退 Edge。
pub async fn text_to_speech_cn(text: &str, voice: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let form = [
        (
            "language",
            "\u{4e2d}\u{6587}\u{666e}\u{901a}\u{8bdd}\u{7b80}\u{4f53}",
        ),
        (
            "voice",
            if voice.is_empty() {
                DEFAULT_VOICE
            } else {
                voice
            },
        ),
        ("text", text),
        ("role", "0"),
        ("style", "0"),
        ("rate", "0"),
        ("pitch", "0"),
        ("kbitrate", "audio-16khz-32kbitrate-mono-mp3"),
        ("silence", ""),
        ("styledegree", "1"),
        ("user_id", ""),
        ("yzm", ""),
    ];
    let resp = client
        .post("https://www.text-to-speech.cn/getSpeek.php")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/113.0.0.0 Safari/537.36",
        )
        .header("Origin", "https://www.text-to-speech.cn")
        .header("Referer", "https://www.text-to-speech.cn/")
        .form(&form)
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    let url = v
        .get("download")
        .and_then(|d| d.as_str())
        .filter(|u| !u.is_empty())
        .ok_or_else(|| anyhow::anyhow!("textToSpeechCn 响应无 download 字段"))?
        .to_string();
    Ok(url)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EdgeVoice {
    /// 显示名（中文）
    pub name: &'static str,
    /// 语音 ID（SSML voice name）
    pub value: &'static str,
    /// 语言区域（zh-CN / en-US）
    pub locale: &'static str,
    /// 性别
    pub gender: &'static str,
}

/// GAP 113：getTTSVoices 10 分钟内存缓存（Mutex<Option<(时间戳, 语音列表)>>）
/// 语音列表为静态预置（不含网络请求），缓存主要避免每次请求重复构造/序列化；
/// 10 分钟过期后重建（为未来动态语音源预留结构）
static VOICE_CACHE: std::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<Vec<EdgeVoice>>)>> =
    std::sync::Mutex::new(None);

/// 语音列表缓存 TTL（10 分钟）
const VOICE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// 带 10 分钟内存缓存的语音列表读取（GAP 113）
pub fn edge_voices_cached() -> std::sync::Arc<Vec<EdgeVoice>> {
    let now = std::time::Instant::now();
    if let Ok(mut guard) = VOICE_CACHE.lock() {
        if let Some((ts, voices)) = guard.as_ref() {
            if now.duration_since(*ts) < VOICE_CACHE_TTL {
                return voices.clone();
            }
        }
        let voices = std::sync::Arc::new(edge_voices().to_vec());
        *guard = Some((now, voices.clone()));
        voices
    } else {
        // 锁中毒兜底：直接返回（不缓存）
        std::sync::Arc::new(edge_voices().to_vec())
    }
}

/// 预置 Edge 语音列表（zh-CN 常见 + en-US 若干；完整列表可后续扩充）
pub fn edge_voices() -> &'static [EdgeVoice] {
    &[
        // zh-CN 女声
        EdgeVoice {
            name: "晓晓",
            value: "zh-CN-XiaoxiaoNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓伊",
            value: "zh-CN-XiaoyiNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓辰",
            value: "zh-CN-XiaochenNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓涵",
            value: "zh-CN-XiaohanNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓墨",
            value: "zh-CN-XiaomoNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓萱",
            value: "zh-CN-XiaoxuanNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓颜",
            value: "zh-CN-XiaoyanNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓悠",
            value: "zh-CN-XiaoyouNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓梦",
            value: "zh-CN-XiaomengNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        EdgeVoice {
            name: "晓双",
            value: "zh-CN-XiaoshuangNeural",
            locale: "zh-CN",
            gender: "Female",
        },
        // zh-CN 男声
        EdgeVoice {
            name: "云希",
            value: "zh-CN-YunxiNeural",
            locale: "zh-CN",
            gender: "Male",
        },
        EdgeVoice {
            name: "云扬",
            value: "zh-CN-YunyangNeural",
            locale: "zh-CN",
            gender: "Male",
        },
        EdgeVoice {
            name: "云健",
            value: "zh-CN-YunjianNeural",
            locale: "zh-CN",
            gender: "Male",
        },
        EdgeVoice {
            name: "云夏",
            value: "zh-CN-YunxiaNeural",
            locale: "zh-CN",
            gender: "Male",
        },
        EdgeVoice {
            name: "云枫",
            value: "zh-CN-YunfengNeural",
            locale: "zh-CN",
            gender: "Male",
        },
        // en-US
        EdgeVoice {
            name: "Aria",
            value: "en-US-AriaNeural",
            locale: "en-US",
            gender: "Female",
        },
        EdgeVoice {
            name: "Jenny",
            value: "en-US-JennyNeural",
            locale: "en-US",
            gender: "Female",
        },
        EdgeVoice {
            name: "Ana",
            value: "en-US-AnaNeural",
            locale: "en-US",
            gender: "Female",
        },
        EdgeVoice {
            name: "Michelle",
            value: "en-US-MichelleNeural",
            locale: "en-US",
            gender: "Female",
        },
        EdgeVoice {
            name: "Guy",
            value: "en-US-GuyNeural",
            locale: "en-US",
            gender: "Male",
        },
        EdgeVoice {
            name: "Christopher",
            value: "en-US-ChristopherNeural",
            locale: "en-US",
            gender: "Male",
        },
        EdgeVoice {
            name: "Eric",
            value: "en-US-EricNeural",
            locale: "en-US",
            gender: "Male",
        },
        EdgeVoice {
            name: "Roger",
            value: "en-US-RogerNeural",
            locale: "en-US",
            gender: "Male",
        },
        EdgeVoice {
            name: "Steffan",
            value: "en-US-SteffanNeural",
            locale: "en-US",
            gender: "Male",
        },
    ]
}

/// 默认语音
pub const DEFAULT_VOICE: &str = "zh-CN-XiaoxiaoNeural";
/// 单次合成文本上限（字符）
pub const MAX_TEXT_CHARS: usize = 20_000;
/// 单请求块上限（字符；超过按句切块多次合成）
pub const CHUNK_MAX_CHARS: usize = 2_500;

/// 微软 Edge 语音鉴权 token（固定 TrustedClientToken）
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
/// Windows FILETIME 纪元偏移（秒）：1601-01-01 → 1970-01-01
const WIN_EPOCH_SECS: i64 = 11_644_473_600;
/// Chromium / Edge 版本号（对齐 upstream edge-tts v2 Sec-MS-GEC 握手）
const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";
const SEC_MS_GEC_VERSION: &str = "2-143.0.3650.75";
const EDGE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";
/// WSS 端点
const EDGE_WSS_URL: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
/// 输出音频格式（MP3）
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

const GEC_SHA256_INITIAL: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];
const GEC_SHA256_STATE: [u32; 8] = [
    0xFD9FBEA2, 0x91243CE7, 0xBE499094, 0xCD2E1E0A, 0x6C006F49, 0x04BD35AB, 0xFEB0A7E4, 0x03EFFBD2,
];
const GEC_SHA256_SCHEDULE: [u32; 14] = [
    0xEB24BF9E, 0x1558691F, 0xA0C09EE2, 0x0CFA32BA, 0xA622EE8C, 0xD1DD9491, 0xC0DD25A0, 0xF9E70AB6,
    0xF0C31573, 0x7058CE4E, 0x7188E606, 0x00E5AB33, 0xD258B060, 0x72766564,
];
const GEC_SHA256_K: [u32; 64] = [
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1, 0x923F82A4, 0xAB1C5ED5,
    0xD807AA98, 0x12835B01, 0x243185BE, 0x550C7DC3, 0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174,
    0xE49B69C1, 0xEFBE4786, 0x0FC19DC6, 0x240CA1CC, 0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA,
    0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147, 0x06CA6351, 0x14292967,
    0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13, 0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85,
    0xA2BFE8A1, 0xA81A664B, 0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070,
    0x19A4C116, 0x1E376C08, 0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A, 0x5B9CCA4F, 0x682E6FF3,
    0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208, 0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
];
const GEC_SHA256_ADJ: u32 = 0xC9DC06A7;
const GEC_SIGMA0_ADJ: u32 = 0x059C0B80;
const GEC_SIGMA1_ADJ: u32 = 0x000B92C0;

const GEC_CS64_COEFFS: [(u32, u32); 14] = [
    (0xE82E91B1, 0xB32700D0),
    (0x44DE24C6, 0x88DFF51D),
    (0x3BEB65BC, 0x29514038),
    (0xC89C5933, 0xB328CBDF),
    (0x8D1EC211, 0xD405B4D5),
    (0x314EF5D5, 0x26E55F3C),
    (0xE92491B1, 0x607801C6),
    (0xF0A5B9D5, 0x82B2C1FB),
    (0x06064CF3, 0xFA51A3DA),
    (0xDA79FA60, 0x207CC464),
    (0xBED20560, 0x3C3C74F3),
    (0x89F9C9F8, 0xD2D14B61),
    (0x35FA38C6, 0xBB04A2C1),
    (0xCB49E3C6, 0xC5626167),
];

#[inline]
fn gec_sigma0(v: u32) -> u32 {
    v.rotate_right(7) ^ v.rotate_right(18) ^ (v >> 3)
}

#[inline]
fn gec_sigma1(v: u32) -> u32 {
    v.rotate_right(17) ^ v.rotate_right(19) ^ (v >> 10)
}

fn gec_expand_schedule(words: &mut [u32; 64], first_word: usize) {
    for idx in first_word..64 {
        let s1 = gec_sigma1(words[idx - 2]);
        let s0 = gec_sigma0(words[idx - 15]);
        words[idx] = s1
            .wrapping_add(words[idx - 7])
            .wrapping_add(s0)
            .wrapping_add(words[idx - 16]);
    }
}

fn gec_sha256_rounds(state: [u32; 8], words: &[u32; 64], first_round: usize) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for idx in first_round..64 {
        let k = if first_round == 14 && idx == 14 {
            GEC_SHA256_ADJ
        } else {
            GEC_SHA256_K[idx]
        };
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = g ^ (e & (f ^ g));
        let t1 = h
            .wrapping_add(big_s1)
            .wrapping_add(ch)
            .wrapping_add(k)
            .wrapping_add(words[idx]);
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) | (c & (a | b));
        let t2 = big_s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    [a, b, c, d, e, f, g, h]
}

fn gec_sha256_digest(data: &[u8]) -> [u8; 32] {
    let mut words = [0u32; 64];
    words[14] = u32::from_be_bytes([0, data[0], data[1], data[2]]);
    words[15] = u32::from_be_bytes([data[3], data[4], data[5], data[6]]);
    for idx in 16..30 {
        let mut val = GEC_SHA256_SCHEDULE[idx - 16];
        if idx - 2 >= 14 {
            let s1 = gec_sigma1(words[idx - 2]);
            val = val.wrapping_add(if idx == 16 { s1 ^ GEC_SIGMA1_ADJ } else { s1 });
        }
        if idx - 7 >= 14 {
            val = val.wrapping_add(words[idx - 7]);
        }
        if idx - 15 >= 14 {
            let s0 = gec_sigma0(words[idx - 15]);
            val = val.wrapping_add(if idx == 29 { s0 ^ GEC_SIGMA0_ADJ } else { s0 });
        }
        words[idx] = val;
    }
    words[30] = gec_sigma1(words[28])
        .wrapping_add(words[23])
        .wrapping_add(gec_sigma0(words[15]))
        .wrapping_add(words[14])
        .wrapping_add(GEC_SHA256_ADJ)
        .wrapping_sub(GEC_SHA256_K[14]);
    gec_expand_schedule(&mut words, 31);
    let working = gec_sha256_rounds(GEC_SHA256_STATE, &words, 14);
    let mut state = [0u32; 8];
    for i in 0..8 {
        state[i] = GEC_SHA256_INITIAL[i].wrapping_add(working[i]);
    }

    let length = 57usize + data.len();
    let pad_zeros = (55isize - (length as isize)).rem_euclid(64) as usize;
    let tail = &data[7..];
    let mut padded = Vec::with_capacity(tail.len() + 1 + pad_zeros + 8);
    padded.extend_from_slice(tail);
    padded.push(0x80);
    padded.resize(tail.len() + 1 + pad_zeros, 0);
    padded.extend_from_slice(&((length as u64) * 8).to_be_bytes());

    for block in padded.chunks_exact(64) {
        let mut blk_words = [0u32; 64];
        for (i, w) in block.chunks_exact(4).enumerate() {
            blk_words[i] = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
        }
        gec_expand_schedule(&mut blk_words, 16);
        let working = gec_sha256_rounds(state, &blk_words, 0);
        for i in 0..8 {
            state[i] = state[i].wrapping_add(working[i]);
        }
    }

    let mut out = [0u8; 32];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

fn gec_cs64_digest(data: &[u8], digest: &[u8; 32]) -> (u32, u32) {
    let key_a = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) | 1;
    let key_b = u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) | 1;
    let factors = [
        key_a.wrapping_mul(0x82893933),
        key_b.wrapping_mul(0x82893933),
    ];
    let mut terms: Vec<u32> = GEC_CS64_COEFFS
        .iter()
        .enumerate()
        .map(|(i, &(c, _))| c.wrapping_mul(factors[i % 2]))
        .collect();
    let mut offsets: Vec<u32> = GEC_CS64_COEFFS.iter().map(|&(_, o)| o).collect();

    let mut prefixed = Vec::with_capacity(1 + data.len() + 4);
    prefixed.push(0u8);
    prefixed.extend_from_slice(data);
    while !prefixed.len().is_multiple_of(4) {
        prefixed.push(0);
    }
    let mut le_words: Vec<u32> = prefixed
        .chunks_exact(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    if !le_words.len().is_multiple_of(2) {
        le_words.push(0);
    }

    for (idx, &word) in le_words.iter().enumerate() {
        let mut term = word.wrapping_mul(if idx % 2 == 0 { key_a } else { key_b });
        let mut offset =
            word.rotate_left(16)
                .wrapping_mul(if idx % 2 == 0 { 0xEF0569FB } else { 0xC31713DB });
        if idx == 0 {
            term = term.wrapping_add(0xF2F90B1Au32.wrapping_mul(factors[0]));
            offset = offset.wrapping_add(0x0B1A0000);
        }
        terms.push(term);
        offsets.push(offset);
    }

    let mut swap_acc = 0u32;
    let mut swap_last = 0u32;
    let mut state = 0u32;
    let mut acc = 0u32;
    for idx in (0..terms.len()).step_by(2) {
        let (x, y) = (terms[idx], terms[idx + 1]);
        let t0 = offsets[idx].wrapping_add(x).rotate_left(16);
        let q0 = t0.wrapping_mul(0x76F50F34);
        swap_acc =
            swap_acc.wrapping_add(q0.rotate_left(16).wrapping_mul(0xC3EFEA97).wrapping_add(q0));
        let t1 = offsets[idx + 1].wrapping_add(y).rotate_left(16);
        let q1 = t1.wrapping_mul(0x1881CE3C);
        swap_last = q1.rotate_left(16).wrapping_mul(0x35BD1EC9).wrapping_add(q1);
        swap_acc = swap_acc.wrapping_add(swap_last);

        state = state.wrapping_mul(key_a).wrapping_add(x);
        state = state.rotate_left(16).wrapping_mul(0xCF98B111);
        state = state.rotate_left(16).wrapping_mul(0x87085B9F);
        state = state.rotate_left(16).wrapping_mul(0x12CEB96D);
        state = state.rotate_left(16).wrapping_mul(0x257E1D83);
        acc = acc.wrapping_add(state);
        let mut t = state.wrapping_mul(key_b).wrapping_add(y);
        t = t.rotate_left(16).wrapping_mul(0xA27416F5);
        t = t.rotate_left(16).wrapping_mul(0xD38396FF);
        state = t
            .rotate_left(16)
            .wrapping_mul(0x7C932B89)
            .rotate_left(16)
            .wrapping_mul(0xBFA49F69);
        acc = acc.wrapping_add(state);
    }
    (swap_last ^ state, swap_acc ^ acc)
}

/// 生成 Edge TTS v2 `Sec-MS-GEC` 鉴权令牌（对齐 upstream `edge-tts` CS64 + salted SHA-256）：
/// 返回 `(Sec-MS-GEC, Sec-MS-GEC-Version)`，其中 Version 为 `"2-143.0.3650.75"`。
pub fn sec_ms_gec_at(unix_secs: i64) -> (String, String) {
    let _ = CHROMIUM_FULL_VERSION;
    let win_secs = unix_secs + WIN_EPOCH_SECS;
    let rounded = win_secs - win_secs.rem_euclid(300);
    let ticks_100ns = (rounded as i128) * 10_000_000i128;
    let str_to_hash = format!("{ticks_100ns}{TRUSTED_CLIENT_TOKEN}");
    let bytes = str_to_hash.as_bytes();
    let digest = gec_sha256_digest(bytes);
    let (lo, hi) = gec_cs64_digest(bytes, &digest);
    let mut cs64_bytes = [0u8; 8];
    cs64_bytes[..4].copy_from_slice(&lo.to_le_bytes());
    cs64_bytes[4..].copy_from_slice(&hi.to_le_bytes());
    let cs64_hex: String = cs64_bytes.iter().map(|b| format!("{b:02X}")).collect();
    let sha_hex: String = digest.iter().map(|b| format!("{b:02X}")).collect();
    (
        format!("{cs64_hex}-{sha_hex}"),
        SEC_MS_GEC_VERSION.to_string(),
    )
}

/// 当前时间生成 Sec-MS-GEC
pub fn generate_sec_ms_gec() -> (String, String) {
    sec_ms_gec_at(chrono::Utc::now().timestamp())
}

/// WSS 连接 URL（鉴权 query）
pub fn edge_wss_url(connection_id: &str) -> String {
    let (gec, gec_v) = generate_sec_ms_gec();
    format!(
        "{EDGE_WSS_URL}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={gec}&Sec-MS-GEC-Version={gec_v}&ConnectionId={connection_id}"
    )
}

/// Edge 时间戳头格式（edge-tts date_to_string：GMT+0000 (Coordinated Universal Time)）
pub fn edge_date_string(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string()
}

/// speech.config 消息（连接后第一条）
pub fn build_speech_config(date: &str) -> String {
    let body = serde_json::json!({
        "context": {
            "synthesis": {
                "audio": {
                    "metadataoptions": {
                        "sentenceBoundaryEnabled": "false",
                        "wordBoundaryEnabled": "true",
                    },
                    "outputFormat": OUTPUT_FORMAT,
                }
            }
        }
    });
    format!(
        "X-Timestamp:{date}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{body}"
    )
}

/// 剥离 XML 1.0 非法字符（控制字符保留 \t \n \r）
pub fn sanitize_xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        if c == 0x9
            || c == 0xA
            || c == 0xD
            || (0x20..=0xD7FF).contains(&c)
            || (0xE000..=0xFFFD).contains(&c)
            || (0x10000..=0x10FFFF).contains(&c)
        {
            out.push(ch);
        }
    }
    out
}

/// XML 转义（& < > " '）
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 从语音 ID 提取语言区域（zh-CN-XiaoxiaoNeural → zh-CN）
pub fn voice_locale(voice: &str) -> &str {
    let parts: Vec<&str> = voice.split('-').collect();
    if parts.len() >= 3 && (parts[0].len() == 2) && (parts[1].len() == 2) {
        // 保持原大小写（zh-CN / en-US）
        return &voice[..5];
    }
    "zh-CN"
}

/// 构造 SSML 合成请求消息（Path:ssml）
/// `volume`：prosody volume（默认 +0%）；`style`：mstts express-as 风格（仅 Azure/Edge 支持，
/// 非空时输出 `<mstts:express-as>` 包裹，legacy SSML 语义对齐）
pub fn build_ssml(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> String {
    let request_id = uuid::Uuid::new_v4();
    let date = edge_date_string(chrono::Utc::now());
    let locale = voice_locale(voice);
    let safe_text = xml_escape(&sanitize_xml_text(text));
    let volume = if volume.trim().is_empty() {
        "+0%"
    } else {
        volume.trim()
    };
    let (style_open, style_close) = match style.filter(|s| !s.trim().is_empty()) {
        Some(s) => (
            format!("<mstts:express-as style='{}'>", xml_escape(s.trim())),
            "</mstts:express-as>".to_string(),
        ),
        None => (String::new(), String::new()),
    };
    let ssml = format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' \
         xmlns:mstts='https://www.w3.org/2001/mstts' xml:lang='{locale}'>\
<voice name='{voice}'>{style_open}<prosody pitch='{pitch}' rate='{rate}' volume='{volume}'>\
{safe_text}</prosody>{style_close}</voice></speak>"
    );
    format!(
        "X-RequestId:{request_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{date}Z\r\nPath:ssml\r\n\r\n{ssml}"
    )
}

/// 按句切块（句末标点。！？!?；;\n 处断开，保留标点；单句超长硬切）
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    // 先按句拆分
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            sentences.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        sentences.push(cur);
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut buf_len = 0usize;
    for sentence in sentences {
        let s_len = sentence.chars().count();
        if s_len > max_chars {
            // 单句超长：先 flush 当前块，再硬切
            if buf_len > 0 {
                chunks.push(std::mem::take(&mut buf));
                buf_len = 0;
            }
            let mut piece = String::new();
            let mut piece_len = 0usize;
            for ch in sentence.chars() {
                piece.push(ch);
                piece_len += 1;
                if piece_len >= max_chars {
                    chunks.push(std::mem::take(&mut piece));
                    piece_len = 0;
                }
            }
            if piece_len > 0 {
                buf = piece;
                buf_len = piece_len;
            }
        } else if buf_len + s_len > max_chars {
            chunks.push(std::mem::take(&mut buf));
            buf = sentence;
            buf_len = s_len;
        } else {
            buf.push_str(&sentence);
            buf_len += s_len;
        }
    }
    if buf_len > 0 {
        chunks.push(buf);
    }
    chunks
}

/// 解析二进制音频消息：2 字节大端头长 + JSON 头 + 音频帧（无头/短帧返回 None）
pub fn split_audio_frame(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 2 {
        return None;
    }
    let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    if data.len() < 2 + header_len {
        return None;
    }
    Some(&data[2 + header_len..])
}

/// Edge TTS 合成（文本 → MP3 字节流）：长文本按句分块逐块合成后拼接
pub async fn edge_synthesize(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> Result<Vec<u8>> {
    let text = sanitize_xml_text(text);
    if text.trim().is_empty() {
        return Err(anyhow!("合成文本不能为空"));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(anyhow!("合成文本过长（最多 {MAX_TEXT_CHARS} 字符）"));
    }
    let chunks = chunk_text(&text, CHUNK_MAX_CHARS);
    let mut audio = Vec::new();
    for chunk in chunks {
        let chunk_audio = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            edge_synthesize_chunk(&chunk, voice, rate, pitch, volume, style),
        )
        .await
        .map_err(|_| anyhow!("Edge TTS 合成超时"))??;
        audio.extend_from_slice(&chunk_audio);
    }
    Ok(audio)
}

/// 构造带完整 WebSocket 握手头（含 `Sec-WebSocket-Key` / `MUID` Cookie / Edge UA）的 WSS 请求
fn build_edge_ws_request(url: &str) -> Result<http::Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url
        .into_client_request()
        .map_err(|e| anyhow!("构造 WSS 请求失败: {e}"))?;
    let muid = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
    let headers = request.headers_mut();
    headers.insert("Pragma", http::HeaderValue::from_static("no-cache"));
    headers.insert("Cache-Control", http::HeaderValue::from_static("no-cache"));
    headers.insert(
        "Origin",
        http::HeaderValue::from_static("chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"),
    );
    headers.insert(
        "User-Agent",
        http::HeaderValue::from_static(EDGE_USER_AGENT),
    );
    headers.insert(
        "Accept-Encoding",
        http::HeaderValue::from_static("gzip, deflate, br, zstd"),
    );
    headers.insert(
        "Accept-Language",
        http::HeaderValue::from_static("en-US,en;q=0.9"),
    );
    if let Ok(cookie_val) = http::HeaderValue::from_str(&format!("muid={muid};")) {
        headers.insert("Cookie", cookie_val);
    }
    Ok(request)
}

/// 单块 Edge TTS 合成（一次 WSS 会话）
async fn edge_synthesize_chunk(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> Result<Vec<u8>> {
    let connection_id = uuid::Uuid::new_v4().simple().to_string();
    let url = edge_wss_url(&connection_id);
    let request = build_edge_ws_request(&url)?;

    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| anyhow!("Edge TTS 连接失败: {e}"))?;
    let (mut sink, mut stream) = ws.split();

    // 1. speech.config
    let date = edge_date_string(chrono::Utc::now());
    sink.send(tokio_tungstenite::tungstenite::Message::Text(
        build_speech_config(&date),
    ))
    .await
    .map_err(|e| anyhow!("发送 speech.config 失败: {e}"))?;

    // 2. SSML 合成请求
    sink.send(tokio_tungstenite::tungstenite::Message::Text(build_ssml(
        text, voice, rate, pitch, volume, style,
    )))
    .await
    .map_err(|e| anyhow!("发送合成请求失败: {e}"))?;

    // 3. 读音频帧直到 turn.end
    let mut audio: Vec<u8> = Vec::new();
    while let Some(msg) = stream.next().await {
        let msg = msg.map_err(|e| anyhow!("Edge TTS 接收失败: {e}"))?;
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                if t.as_str().contains("Path:turn.end") {
                    break;
                }
            }
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                if let Some(payload) = split_audio_frame(&b) {
                    audio.extend_from_slice(payload);
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    if audio.is_empty() {
        return Err(anyhow!("Edge TTS 未返回音频数据"));
    }
    Ok(audio)
}

/// TTS 流式音频字节流（供 `axum::body::Body::from_stream` 零缓冲透传）
pub type AudioByteStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send>,
>;

/// 建立单段 Edge TTS WebSocket 会话（发送 `speech.config` + `ssml` 后返回消息流）
async fn edge_open_ws_chunk(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> Result<
    impl futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin
        + Send
        + 'static,
> {
    let connection_id = uuid::Uuid::new_v4().simple().to_string();
    let url = edge_wss_url(&connection_id);
    let request = build_edge_ws_request(&url)?;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| anyhow!("Edge TTS 连接失败: {e}"))?;

    let date = edge_date_string(chrono::Utc::now());
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        build_speech_config(&date),
    ))
    .await
    .map_err(|e| anyhow!("发送 speech.config 失败: {e}"))?;

    ws.send(tokio_tungstenite::tungstenite::Message::Text(build_ssml(
        text, voice, rate, pitch, volume, style,
    )))
    .await
    .map_err(|e| anyhow!("发送合成请求失败: {e}"))?;

    Ok(ws)
}

/// 从 Edge WebSocket 消息流读取第一帧非空音频数据（用于首字节校验，确保错误在返回 HTTP 200 头之前暴露）
async fn read_first_audio_frame<S>(ws: &mut S) -> Result<axum::body::Bytes>
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| anyhow!("Edge TTS 接收失败: {e}"))?;
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                if t.as_str().contains("Path:turn.end") {
                    break;
                }
            }
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                if let Some(payload) = split_audio_frame(&b).filter(|p| !p.is_empty()) {
                    return Ok(axum::body::Bytes::copy_from_slice(payload));
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    Err(anyhow!("Edge TTS 未返回音频数据"))
}

/// 将单段 Edge WebSocket 剩余音频帧流式转发至通道；返回 `true` 表示该段正常收尾，`false` 表示下游断开或出错
async fn forward_ws_audio_frames<S>(
    mut ws: S,
    tx: &tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
) -> bool
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let next = tokio::select! {
            _ = tx.closed() => return false,
            _ = tokio::time::sleep_until(deadline) => {
                let _ = tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Edge TTS 合成超时",
                    )))
                    .await;
                return false;
            }
            item = ws.next() => item,
        };
        let Some(msg) = next else {
            return true;
        };
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                if t.as_str().contains("Path:turn.end") {
                    return true;
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => {
                if let Some(payload) = split_audio_frame(&b).filter(|p| !p.is_empty()) {
                    if tx
                        .send(Ok(axum::body::Bytes::copy_from_slice(payload)))
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => return true,
            Ok(_) => {}
            Err(e) => {
                let _ = tx
                    .send(Err(std::io::Error::other(format!(
                        "Edge TTS 接收失败: {e}"
                    ))))
                    .await;
                return false;
            }
        }
    }
}

/// 将已读取首帧的首段会话与后续文本段落串联为连续 `AudioByteStream`
fn build_edge_audio_stream<S, F, Fut>(
    first_frame: axum::body::Bytes,
    first_ws: S,
    rest_chunks: Vec<String>,
    open_chunk: F,
) -> AudioByteStream
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin
        + Send
        + 'static,
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(axum::body::Bytes, S)>> + Send,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        if tx.send(Ok(first_frame)).await.is_err() {
            return;
        }
        if !forward_ws_audio_frames(first_ws, &tx).await {
            return;
        }
        for chunk in rest_chunks {
            if tx.is_closed() {
                return;
            }
            match tokio::time::timeout(std::time::Duration::from_secs(30), open_chunk(chunk)).await
            {
                Ok(Ok((frame, ws))) => {
                    if tx.send(Ok(frame)).await.is_err() {
                        return;
                    }
                    if !forward_ws_audio_frames(ws, &tx).await {
                        return;
                    }
                }
                Ok(Err(e)) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    return;
                }
                Err(_) => {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "Edge TTS 合成超时",
                        )))
                        .await;
                    return;
                }
            }
        }
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Box::pin(stream)
}

/// Edge TTS 真流式合成（极低首字延迟）：
/// 先建立首段 WSS 连接并读取到第一帧非空音频数据（网络不可达/音色非法/空文本在返回 HTTP 200 头前报错），
/// 拿到首帧后立即返回字节流，在后台按序流式推送首段剩余帧与后续段落音频帧。
pub async fn edge_synthesize_stream(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> Result<AudioByteStream> {
    let text = sanitize_xml_text(text);
    if text.trim().is_empty() {
        return Err(anyhow!("合成文本不能为空"));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(anyhow!("合成文本过长（最多 {MAX_TEXT_CHARS} 字符）"));
    }
    let mut chunks = chunk_text(&text, CHUNK_MAX_CHARS).into_iter();
    let Some(first_chunk) = chunks.next() else {
        return Err(anyhow!("合成文本不能为空"));
    };
    let rest_chunks: Vec<String> = chunks.collect();

    let (first_frame, first_ws) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut ws = edge_open_ws_chunk(&first_chunk, voice, rate, pitch, volume, style).await?;
        let first_frame = read_first_audio_frame(&mut ws).await?;
        Ok::<_, anyhow::Error>((first_frame, ws))
    })
    .await
    .map_err(|_| anyhow!("Edge TTS 合成超时"))??;

    let voice_owned = voice.to_string();
    let rate_owned = rate.to_string();
    let pitch_owned = pitch.to_string();
    let volume_owned = volume.to_string();
    let style_owned = style.map(str::to_string);

    Ok(build_edge_audio_stream(
        first_frame,
        first_ws,
        rest_chunks,
        move |chunk| {
            let voice = voice_owned.clone();
            let rate = rate_owned.clone();
            let pitch = pitch_owned.clone();
            let volume = volume_owned.clone();
            let style = style_owned.clone();
            async move {
                let mut ws =
                    edge_open_ws_chunk(&chunk, &voice, &rate, &pitch, &volume, style.as_deref())
                        .await?;
                let frame = read_first_audio_frame(&mut ws).await?;
                Ok((frame, ws))
            }
        },
    ))
}

/// 百分比编码（URL query / {text} 占位符替换）
fn urlencode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// HttpTTS 请求 URL 构造（legado 语义）：
/// - URL 含 {text}/{voice}/{rate}/{pitch} 占位符 → 逐个替换（值百分比编码）
/// - 无 {text} 占位符 → 追加 query：text=...（已有 query 用 & 连接）
pub fn build_http_tts_url(
    url: &str,
    text: &str,
    voice: Option<&str>,
    rate: Option<&str>,
    pitch: Option<&str>,
    volume: Option<&str>,
) -> String {
    let mut out = url.to_string();
    if out.contains("{text}") {
        out = out.replace("{text}", &urlencode(text));
        if let Some(v) = voice {
            out = out.replace("{voice}", &urlencode(v));
        }
        if let Some(r) = rate {
            out = out.replace("{rate}", &urlencode(r));
        }
        if let Some(p) = pitch {
            out = out.replace("{pitch}", &urlencode(p));
        }
        if let Some(v) = volume {
            out = out.replace("{volume}", &urlencode(v));
        }
        return out;
    }
    // 无占位符：追加 query
    let sep = if out.contains('?') { '&' } else { '?' };
    out.push_str(&format!("{sep}text={}", urlencode(text)));
    if let Some(v) = voice {
        out.push_str(&format!("&voice={}", urlencode(v)));
    }
    if let Some(r) = rate {
        out.push_str(&format!("&rate={}", urlencode(r)));
    }
    if let Some(p) = pitch {
        out.push_str(&format!("&pitch={}", urlencode(p)));
    }
    if let Some(v) = volume {
        out.push_str(&format!("&volume={}", urlencode(v)));
    }
    out
}

/// HttpTTS 合成调用（GET {url}?text=...；URL 超长且无占位符时改 POST form）
/// 返回音频字节（MP3 等，原样透传）
pub async fn http_tts_synthesize(
    url: &str,
    text: &str,
    voice: Option<&str>,
    rate: Option<&str>,
    pitch: Option<&str>,
    volume: Option<&str>,
) -> Result<Vec<u8>> {
    if text.trim().is_empty() {
        return Err(anyhow!("合成文本不能为空"));
    }
    // P1 SSRF：HttpTTS 引擎地址同样做公网校验（DNS 解析后——拒绝私网/回环/169.254 等）
    crate::service::crawler::validate_public_target(url).await?;
    let client =
        crate::service::crawler::http_client_builder(60, reqwest::redirect::Policy::limited(5))
            .map_err(|e| anyhow!("HttpTTS 客户端初始化失败: {e}"))?;

    let has_placeholder = url.contains("{text}");
    let final_url = build_http_tts_url(url, text, voice, rate, pitch, volume);
    let resp = if !has_placeholder && final_url.len() > 2048 {
        // URL 超长 → POST form（text/voice/rate/pitch）
        let mut form: Vec<(&str, String)> = vec![("text", text.to_string())];
        if let Some(v) = voice {
            form.push(("voice", v.to_string()));
        }
        if let Some(r) = rate {
            form.push(("rate", r.to_string()));
        }
        if let Some(p) = pitch {
            form.push(("pitch", p.to_string()));
        }
        if let Some(v) = volume {
            form.push(("volume", v.to_string()));
        }
        client
            .post(url)
            .form(&form)
            .send()
            .await
            .map_err(|e| anyhow!("HttpTTS 请求失败: {e}"))?
    } else {
        client
            .get(&final_url)
            .send()
            .await
            .map_err(|e| anyhow!("HttpTTS 请求失败: {e}"))?
    };
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow!("HttpTTS 读取响应失败: {e}"))?;
    if bytes.is_empty() {
        return Err(anyhow!("HttpTTS 未返回音频数据"));
    }
    Ok(bytes.to_vec())
}

/// F4：HttpTTS API 引擎合成（legacy getSpeakStream 对齐——type=api 时按名解析的
/// 听书源驱动）。返回 (音频字节, 实际 Content-Type)。
/// - `{{speakText}}`/`{text}` → 朗读文本；`{{speakSpeed}}`/`{{speed}}` → 语速整数
/// - GET query 中 speakText 百分号编码；POST body 原文替换
/// - header 注入（tts.header JSON）+ 登录头（login_header_for）
/// - 响应 application/json → 报错（含响应体）；contentType 正则不匹配 → 报错
/// - 超时/连接类错误重试 ≤5 次
pub async fn http_tts_api_synthesize(
    ns: &str,
    tts: &crate::model::HttpTts,
    text: &str,
    speed: i64,
) -> Result<(Vec<u8>, Option<String>)> {
    use crate::service::search::split_url_suffix;
    if text.trim().is_empty() {
        return Err(anyhow!("合成文本不能为空"));
    }
    let raw = tts.url.trim();
    let (mut url, suffix) = split_url_suffix(raw);
    // 占位符替换：URL 部分 speakText 百分号编码；body 部分原文
    let enc_text = form_urlencoded(text);
    url = url
        .replace("{{speakText}}", &enc_text)
        .replace("{{text}}", &enc_text)
        .replace("{{speakSpeed}}", &speed.to_string())
        .replace("{{speed}}", &speed.to_string());
    crate::service::crawler::validate_public_target(&url).await?;

    // method/body：后缀显式优先；无后缀且 URL 含 POST 型占位符特征时仍按 GET 处理
    let (method, body): (String, Option<String>) = match (
        suffix.method.as_deref().map(|m| m.to_ascii_uppercase()),
        suffix.body.clone(),
    ) {
        (Some(m), b) => (m, b),
        _ => ("GET".to_string(), None),
    };
    let body = body.map(|b| {
        b.replace("{{speakText}}", text)
            .replace("{{text}}", text)
            .replace("{{speakSpeed}}", &speed.to_string())
            .replace("{{speed}}", &speed.to_string())
    });

    // headers：tts.header JSON + 登录头覆盖
    let mut headers: std::collections::HashMap<String, String> =
        crate::service::crawler::parse_header(tts.header.as_deref().unwrap_or(""));
    if let Some(lh) = crate::service::crawler::login_header_for(ns, &tts.url).await {
        for (k, v) in crate::service::crawler::parse_header(&lh) {
            headers.insert(k, v);
        }
    }

    let client =
        crate::service::crawler::http_client_builder(60, reqwest::redirect::Policy::limited(5))
            .map_err(|e| anyhow!("HttpTTS 客户端初始化失败: {e}"))?;

    // 重试 ≤5（超时/连接类错误）；其他错误一次即出
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let req = match method.as_str() {
            "POST" => client
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded"),
            _ => client.get(&url),
        };
        let mut req = req;
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(b) = &body {
            req = req.body(b.clone());
        }
        let send = req.send().await;
        let resp = match send {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("{e}");
                let transient =
                    msg.contains("timed out") || msg.contains("timeout") || msg.contains("connect");
                if transient && attempt <= 5 {
                    continue;
                }
                return Err(anyhow!("TTS 下载错误: {msg}"));
            }
        };
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("TTS 读取响应失败: {e}"))?
            .to_vec();
        let ct_lower = ct.as_deref().unwrap_or_default().to_ascii_lowercase();
        if ct_lower.starts_with("application/json") {
            let body_text = String::from_utf8_lossy(&bytes);
            return Err(anyhow!("{}", &body_text[..body_text.len().min(300)]));
        }
        if let Some(expected) = tts.content_type.as_deref() {
            if !expected.trim().is_empty() {
                let ok = crate::util::regex::Regex::new(expected)
                    .map(|re| re.is_match(ct.as_deref().unwrap_or_default()))
                    .unwrap_or(true);
                if !ok {
                    let preview = String::from_utf8_lossy(&bytes);
                    return Err(anyhow!(
                        "TTS服务器返回错误：{}",
                        &preview[..preview.len().min(300)]
                    ));
                }
            }
        }
        if bytes.is_empty() {
            return Err(anyhow!("HttpTTS 未返回音频数据"));
        }
        let _ = suffix;
        return Ok((bytes, ct));
    }
}

/// 按 UTF-8 字符边界安全截断错误预览，避免中文截断 panic
fn truncate_utf8_preview(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes.min(s.len()))]
}

/// 尝试将上游 JSON 包装音频（`audio` / `audioContent` / `mp3` / `speech` / `data` 或 `data:audio/...;base64,...`）
/// 解码为音频字节；若为 JSON 错误响应则返回 `None`。
fn decode_json_wrapped_audio(bytes: &[u8]) -> Option<(axum::body::Bytes, Option<String>)> {
    use base64::Engine as _;

    let val: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = val.as_object()?;
    if obj.get("isSuccess").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    if obj
        .get("error")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        || obj
            .get("errorMsg")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    {
        return None;
    }

    let raw_b64 = obj
        .get("audioContent")
        .and_then(|v| v.as_str())
        .or_else(|| obj.get("audio").and_then(|v| v.as_str()))
        .or_else(|| obj.get("mp3").and_then(|v| v.as_str()))
        .or_else(|| obj.get("speech").and_then(|v| v.as_str()))
        .or_else(|| {
            obj.get("data").and_then(|d| {
                d.as_str().or_else(|| {
                    d.as_object().and_then(|m| {
                        m.get("audioContent")
                            .or_else(|| m.get("audio"))
                            .and_then(|v| v.as_str())
                    })
                })
            })
        })?;

    let trimmed = raw_b64.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (data_uri_mime, payload) = if let Some(rest) = trimmed.strip_prefix("data:") {
        let (meta, b64) = rest.split_once(";base64,")?;
        let mime = (!meta.trim().is_empty()).then(|| meta.trim().to_string());
        (mime, b64.trim())
    } else {
        (None, trimmed)
    };

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload))
        .ok()?;
    if decoded.is_empty() {
        return None;
    }

    let ct = data_uri_mime.or_else(|| {
        obj.get("contentType")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    });
    Some((axum::body::Bytes::from(decoded), ct))
}

/// HttpTTS API 引擎真流式合成：
/// 发起上游请求并校验 HTTP 状态码成功、拿到 `Content-Type` 后：
/// - 若上游返回 JSON 包装音频则自动回退缓冲解码为单帧字节流（JSON 错误体仍返回 `Err`）；
/// - 若上游直接返回音频流则零缓冲映射 `resp.bytes_stream()` 立即返回！
pub async fn http_tts_api_synthesize_stream(
    ns: &str,
    tts: &crate::model::HttpTts,
    text: &str,
    speed: i64,
) -> Result<(AudioByteStream, Option<String>)> {
    use crate::service::search::split_url_suffix;
    if text.trim().is_empty() {
        return Err(anyhow!("合成文本不能为空"));
    }
    let raw = tts.url.trim();
    let (mut url, suffix) = split_url_suffix(raw);
    let enc_text = form_urlencoded(text);
    url = url
        .replace("{{speakText}}", &enc_text)
        .replace("{{text}}", &enc_text)
        .replace("{{speakSpeed}}", &speed.to_string())
        .replace("{{speed}}", &speed.to_string());
    crate::service::crawler::validate_public_target(&url).await?;

    let (method, body): (String, Option<String>) = match (
        suffix.method.as_deref().map(|m| m.to_ascii_uppercase()),
        suffix.body.clone(),
    ) {
        (Some(m), b) => (m, b),
        _ => ("GET".to_string(), None),
    };
    let body = body.map(|b| {
        b.replace("{{speakText}}", text)
            .replace("{{text}}", text)
            .replace("{{speakSpeed}}", &speed.to_string())
            .replace("{{speed}}", &speed.to_string())
    });

    let mut headers: std::collections::HashMap<String, String> =
        crate::service::crawler::parse_header(tts.header.as_deref().unwrap_or(""));
    if let Some(lh) = crate::service::crawler::login_header_for(ns, &tts.url).await {
        for (k, v) in crate::service::crawler::parse_header(&lh) {
            headers.insert(k, v);
        }
    }

    let client =
        crate::service::crawler::http_client_builder(60, reqwest::redirect::Policy::limited(5))
            .map_err(|e| anyhow!("HttpTTS 客户端初始化失败: {e}"))?;

    let mut attempt = 0u32;
    let resp = loop {
        attempt += 1;
        let mut req = match method.as_str() {
            "POST" => client
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded"),
            _ => client.get(&url),
        };
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(b) = &body {
            req = req.body(b.clone());
        }
        match req.send().await {
            Ok(r) => break r,
            Err(e) => {
                let msg = format!("{e}");
                let transient =
                    msg.contains("timed out") || msg.contains("timeout") || msg.contains("connect");
                if transient && attempt <= 5 {
                    continue;
                }
                return Err(anyhow!("TTS 下载错误: {msg}"));
            }
        }
    };

    let status = resp.status();
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if !status.is_success() {
        let bytes = resp.bytes().await.unwrap_or_default();
        let preview = String::from_utf8_lossy(&bytes);
        return Err(anyhow!(
            "TTS服务器返回 HTTP {}: {}",
            status.as_u16(),
            truncate_utf8_preview(&preview, 300)
        ));
    }

    let ct_lower = ct.as_deref().unwrap_or_default().to_ascii_lowercase();
    if ct_lower.starts_with("application/json") {
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("TTS 读取响应失败: {e}"))?;
        if let Some((audio, decoded_ct)) = decode_json_wrapped_audio(&bytes) {
            let stream: AudioByteStream = Box::pin(futures::stream::once(async move { Ok(audio) }));
            return Ok((
                stream,
                decoded_ct.or_else(|| Some("audio/mpeg".to_string())),
            ));
        }
        let body_text = String::from_utf8_lossy(&bytes);
        return Err(anyhow!("{}", truncate_utf8_preview(&body_text, 300)));
    }

    if let Some(expected) = tts.content_type.as_deref() {
        if !expected.trim().is_empty() {
            let ok = crate::util::regex::Regex::new(expected)
                .map(|re| re.is_match(ct.as_deref().unwrap_or_default()))
                .unwrap_or(true);
            if !ok {
                let bytes = resp.bytes().await.unwrap_or_default();
                let preview = String::from_utf8_lossy(&bytes);
                return Err(anyhow!(
                    "TTS服务器返回错误：{}",
                    truncate_utf8_preview(&preview, 300)
                ));
            }
        }
    }

    let stream = resp
        .bytes_stream()
        .map(|res| res.map_err(|e| std::io::Error::other(format!("TTS 流读取失败: {e}"))));
    Ok((Box::pin(stream), ct))
}

/// 表单/查询编码（%XX，空格→%20）
fn form_urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P1 SSRF：HttpTTS 引擎地址拒绝私网/回环/链路本地（DNS 解析后校验，错误返回）
    #[tokio::test]
    async fn test_http_tts_synthesize_rejects_private_url() {
        let _g = crate::service::crawler::ssrf_allow_private_guard(false);
        for url in [
            "http://127.0.0.1:1/tts",
            "http://192.168.1.1/tts",
            "http://169.254.169.254/tts",
            "http://[::1]:1/tts",
        ] {
            let err = http_tts_synthesize(url, "你好", None, None, None, None)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("已拦截"),
                "HttpTTS 应拦截私网地址（{url}）: {err}"
            );
        }
    }

    /// P1 SSRF：HttpTTS 合成文本为空仍先报文本错误（校验顺序不破坏既有语义）
    #[tokio::test]
    async fn test_http_tts_synthesize_empty_text_first() {
        let _g = crate::service::crawler::ssrf_allow_private_guard(false);
        let err = http_tts_synthesize("http://127.0.0.1:1/tts", "  ", None, None, None, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("合成文本不能为空"),
            "空文本应先报错: {err}"
        );
    }

    /// 语音列表：非空、含 zh-CN 晓晓 + en-US Aria、字段齐全
    #[test]
    fn test_edge_voices() {
        let voices = edge_voices();
        assert!(!voices.is_empty());
        assert!(voices
            .iter()
            .any(|v| v.value == "zh-CN-XiaoxiaoNeural" && v.name == "晓晓"));
        assert!(voices.iter().any(|v| v.value == "en-US-AriaNeural"));
        for v in voices {
            assert!(!v.value.is_empty() && !v.locale.is_empty() && !v.gender.is_empty());
        }
    }

    /// SSML：voice/prosody 参数 + XML 转义
    #[test]
    fn test_build_ssml() {
        let ssml = build_ssml(
            "你好<世界> & \"书\"",
            "zh-CN-YunxiNeural",
            "+10%",
            "-2Hz",
            "+0%",
            None,
        );
        assert!(ssml.contains("Path:ssml"));
        assert!(ssml.contains("<voice name='zh-CN-YunxiNeural'>"));
        assert!(ssml.contains("pitch='-2Hz' rate='+10%'"));
        assert!(ssml.contains("xml:lang='zh-CN'"));
        // XML 转义
        assert!(ssml.contains("你好&lt;世界&gt; &amp; &quot;书&quot;"));
        // en-US 语音 → en-US locale
        let ssml_en = build_ssml("hello", "en-US-JennyNeural", "+0%", "+0Hz", "+5%", None);
        assert!(ssml_en.contains("xml:lang='en-US'"));
        assert!(ssml_en.contains("volume='+5%'"));
        // express-as style（legacy Azure 语义）
        let ssml_style = build_ssml(
            "hi",
            "zh-CN-XiaoxiaoNeural",
            "+0%",
            "+0Hz",
            "+0%",
            Some("cheerful"),
        );
        assert!(ssml_style.contains("<mstts:express-as style='cheerful'>"));
        assert!(ssml_style.contains("</mstts:express-as>"));
    }

    /// 非法 XML 字符剥离 + 转义（0x7F 属 XML 1.0 合法范围，保留）
    #[test]
    fn test_sanitize_and_escape() {
        let s = sanitize_xml_text("a\u{0}\u{1}b\u{7f}\t\n\u{8}");
        assert_eq!(s, "ab\u{7f}\t\n");
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    /// Sec-MS-GEC v2：与 upstream edge-tts drm.py 已知参考向量逐位一致
    #[test]
    fn test_sec_ms_gec() {
        let (stamp0, ver0) = sec_ms_gec_at(0);
        assert_eq!(ver0, "2-143.0.3650.75");
        assert_eq!(
            stamp0,
            "52BC7A17B50DFEC9-28C4394775C8751CD2F6859C030C06DB1489ACAB3B3A7651831FC26895177ED6"
        );
        let (stamp1, ver1) = sec_ms_gec_at(1_763_780_000);
        assert_eq!(ver1, "2-143.0.3650.75");
        assert_eq!(
            stamp1,
            "97122B5B45094737-6C104FE4BF8E6082350C35B1B3677D7B61643F3A25658449478873EB7BF061D4"
        );
        // 5 分钟窗口内（0..299 秒）应落入同一刻度
        assert_eq!(sec_ms_gec_at(0), sec_ms_gec_at(299));
        assert_ne!(sec_ms_gec_at(0), sec_ms_gec_at(300));
        // WSS 请求头自动携带 Sec-WebSocket-Key
        let url = edge_wss_url("conn1");
        assert!(url.starts_with(
            "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?"
        ));
        assert!(url.contains("TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4"));
        assert!(url.contains("Sec-MS-GEC=") && url.contains("Sec-MS-GEC-Version=2-143.0.3650.75"));
        let req = build_edge_ws_request(&url).expect("应能构造完整 WSS 请求");
        assert!(req.headers().contains_key("sec-websocket-key"));
        assert!(req.headers().contains_key("cookie"));
    }

    /// speech.config：输出格式 audio-24khz-48kbitrate-mono-mp3
    #[test]
    fn test_build_speech_config() {
        let msg =
            build_speech_config("Fri Sep 13 2024 08:00:00 GMT+0000 (Coordinated Universal Time)");
        assert!(msg.starts_with(
            "X-Timestamp:Fri Sep 13 2024 08:00:00 GMT+0000 (Coordinated Universal Time)"
        ));
        assert!(msg.contains("Path:speech.config"));
        assert!(msg.contains("audio-24khz-48kbitrate-mono-mp3"));
        assert!(msg.contains("outputFormat"));
    }

    /// 时间戳格式（edge-tts 兼容）
    #[test]
    fn test_edge_date_string() {
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
        assert_eq!(
            edge_date_string(dt),
            "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)"
        );
    }

    /// 分块：短文本 1 块；长文本按句分块且每块不超上限；单句超长硬切
    #[test]
    fn test_chunk_text() {
        assert_eq!(chunk_text("", 100), Vec::<String>::new());
        let short = chunk_text("你好。世界！", 100);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0], "你好。世界！");

        // 300 句 × 20 字符，max 100 → 每块 5 句
        let long: String = (0..300).map(|i| format!("第{i}句的内容。")).collect();
        let chunks = chunk_text(&long, 100);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 100, "块超长: {c}");
        }
        let joined: String = chunks.concat();
        assert_eq!(joined, long, "分块拼接应还原原文");

        // 单句超长硬切
        let single = "超".repeat(300);
        let chunks = chunk_text(&single, 100);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), single);
    }

    /// 音频帧解析：2 字节头长 + 头 + 数据
    #[test]
    fn test_split_audio_frame() {
        let header = br#"{"Path":"audio"}"#;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(header.len() as u16).to_be_bytes());
        frame.extend_from_slice(header);
        frame.extend_from_slice(b"ID3MP3DATA");
        assert_eq!(split_audio_frame(&frame), Some(&b"ID3MP3DATA"[..]));
        // 短帧/无头
        assert_eq!(split_audio_frame(b""), None);
        assert_eq!(split_audio_frame(b"\x00"), None);
        assert_eq!(split_audio_frame(&[0, 10, 1]), None);
        // 零头长 → 全量数据
        assert_eq!(split_audio_frame(&[0, 0, 1, 2, 3]), Some(&[1, 2, 3][..]));
    }

    /// HttpTTS URL：{text} 占位符替换 / query 追加 / 已有 query 的 & 连接
    #[test]
    fn test_build_http_tts_url() {
        let with_ph = build_http_tts_url(
            "https://tts.example.com/say?t={text}&v={voice}",
            "你好 world",
            Some("zh-CN-XiaoxiaoNeural"),
            Some("+10%"),
            Some("-2Hz"),
            Some("+0%"),
        );
        assert_eq!(
            with_ph,
            "https://tts.example.com/say?t=%E4%BD%A0%E5%A5%BD%20world&v=zh-CN-XiaoxiaoNeural"
        );
        assert!(!with_ph.contains("{text}"));
        assert!(!with_ph.contains("{voice}"), "{{voice}} 未替换");

        let no_ph = build_http_tts_url(
            "https://tts.example.com/say",
            "你好",
            None,
            None,
            None,
            None,
        );
        assert_eq!(no_ph, "https://tts.example.com/say?text=%E4%BD%A0%E5%A5%BD");

        let with_query = build_http_tts_url(
            "https://tts.example.com/say?a=1",
            "hi",
            None,
            None,
            None,
            None,
        );
        assert_eq!(with_query, "https://tts.example.com/say?a=1&text=hi");

        // 未提供的可选占位符不追加
        let partial = build_http_tts_url("https://t.com/{text}", "x", None, None, None, None);
        assert_eq!(partial, "https://t.com/x");
    }

    /// voice_locale 推导
    #[test]
    fn test_voice_locale() {
        assert_eq!(voice_locale("zh-CN-XiaoxiaoNeural"), "zh-CN");
        assert_eq!(voice_locale("en-US-JennyNeural"), "en-US");
        assert_eq!(voice_locale("weird"), "zh-CN");
    }

    /// GAP 113：语音列表 10 分钟内存缓存——TTL 内两次读取命中同一 Arc（同一实例），
    /// 内容与静态列表一致
    #[test]
    fn test_edge_voices_cached() {
        // 清空缓存（测试隔离）
        *VOICE_CACHE.lock().unwrap() = None;
        let a = edge_voices_cached();
        let b = edge_voices_cached();
        assert!(!a.is_empty());
        assert!(std::sync::Arc::ptr_eq(&a, &b), "TTL 内应命中同一缓存实例");
        assert_eq!(a.len(), edge_voices().len());
        assert_eq!(a[0].value, "zh-CN-XiaoxiaoNeural");
        // 与静态列表一致（逐项对比）
        for (cached, static_v) in a.iter().zip(edge_voices().iter()) {
            assert_eq!(cached.value, static_v.value);
        }
        *VOICE_CACHE.lock().unwrap() = None;
    }

    /// 流式 Edge TTS：首帧为空/空文本在返回 Stream 前报错；多段音频帧按序流式无缝衔接
    #[tokio::test]
    async fn test_edge_stream_multi_chunk_and_first_frame_error() {
        use tokio_tungstenite::tungstenite::{Error as WsError, Message};

        #[allow(clippy::result_large_err)]
        fn make_audio_msg(payload: &[u8]) -> Result<Message, WsError> {
            let header = b"Path:audio\r\n";
            let mut frame = Vec::with_capacity(2 + header.len() + payload.len());
            frame.extend_from_slice(&(header.len() as u16).to_be_bytes());
            frame.extend_from_slice(header);
            frame.extend_from_slice(payload);
            Ok(Message::Binary(frame))
        }

        // 1. 空文本直接前置报错
        let err = edge_synthesize_stream("   ", DEFAULT_VOICE, "+0%", "+0Hz", "+0%", None)
            .await
            .err()
            .expect("空文本应返回错误");
        assert!(err.to_string().contains("合成文本不能为空"), "err={err}");

        // 2. 首段未产出音频即 turn.end → 首帧预读报错（返回 HTTP 200 头之前可触发回退）
        let mut empty_ws = futures::stream::iter(vec![Ok::<Message, WsError>(Message::Text(
            "Path:turn.end".to_string(),
        ))]);
        let err = read_first_audio_frame(&mut empty_ws).await.unwrap_err();
        assert!(err.to_string().contains("未返回音频数据"), "err={err}");

        // 3. 多段落流式推送：首段首帧 + 首段剩余帧 + 次段全部帧按序输出
        let mut ws1 = futures::stream::iter(vec![
            Ok::<Message, WsError>(Message::Text("Path:turn.start".to_string())),
            make_audio_msg(b"frame-1a"),
            make_audio_msg(b"frame-1b"),
            Ok(Message::Text("Path:turn.end".to_string())),
        ]);
        let first_frame = read_first_audio_frame(&mut ws1).await.unwrap();
        assert_eq!(&first_frame[..], b"frame-1a");

        let mut stream = build_edge_audio_stream(
            first_frame,
            ws1,
            vec!["second-chunk".to_string()],
            |_chunk| async move {
                let mut ws2 = futures::stream::iter(vec![
                    make_audio_msg(b"frame-2a"),
                    make_audio_msg(b"frame-2b"),
                    Ok(Message::Text("Path:turn.end".to_string())),
                ]);
                let f = read_first_audio_frame(&mut ws2).await?;
                Ok((f, ws2))
            },
        );

        let mut chunks: Vec<Vec<u8>> = Vec::new();
        while let Some(item) = stream.next().await {
            chunks.push(item.unwrap().to_vec());
        }
        assert_eq!(
            chunks,
            vec![
                b"frame-1a".to_vec(),
                b"frame-1b".to_vec(),
                b"frame-2a".to_vec(),
                b"frame-2b".to_vec(),
            ]
        );
    }

    /// 流式 HttpTTS API：直通音频分块流 + JSON 包装音频自动回退解码 + HTTP/JSON 错误前置拦截
    #[tokio::test]
    async fn test_http_tts_api_synthesize_stream_streaming_json_and_error() {
        use axum::routing::get;
        use base64::Engine as _;

        let _ssrf = crate::service::crawler::ssrf_allow_private_guard(true);

        let app = axum::Router::new()
            .route(
                "/audio",
                get(|| async {
                    let s = futures::stream::iter(vec![
                        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"CHUNK1")),
                        Ok(axum::body::Bytes::from_static(b"CHUNK2")),
                    ]);
                    axum::response::Response::builder()
                        .status(axum::http::StatusCode::OK)
                        .header(axum::http::header::CONTENT_TYPE, "audio/mpeg")
                        .body(axum::body::Body::from_stream(s))
                        .unwrap()
                }),
            )
            .route(
                "/json-audio",
                get(|| async {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(b"JSON_MP3");
                    axum::Json(serde_json::json!({
                        "isSuccess": true,
                        "audio": b64,
                    }))
                }),
            )
            .route(
                "/json-err",
                get(|| async {
                    axum::Json(serde_json::json!({
                        "isSuccess": false,
                        "errorMsg": "quota exceeded",
                        "data": "NEED_LOGIN",
                    }))
                }),
            )
            .route(
                "/fail",
                get(|| async { (axum::http::StatusCode::BAD_GATEWAY, "upstream error") }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let make_tts = |path: &str| crate::model::HttpTts {
            url: format!("http://{addr}{path}?text={{{{speakText}}}}&speed={{{{speakSpeed}}}}"),
            name: "test-tts".to_string(),
            ..Default::default()
        };

        // 1. 直接音频流透传
        let (mut stream, ct) =
            http_tts_api_synthesize_stream("default", &make_tts("/audio"), "你好", 5)
                .await
                .unwrap();
        assert_eq!(ct.as_deref(), Some("audio/mpeg"));
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(collected, b"CHUNK1CHUNK2");

        // 2. JSON 包装音频自动回退缓冲解码
        let (mut stream, ct) =
            http_tts_api_synthesize_stream("default", &make_tts("/json-audio"), "你好", 5)
                .await
                .unwrap();
        assert_eq!(ct.as_deref(), Some("audio/mpeg"));
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(collected, b"JSON_MP3");

        // 3. JSON 错误响应返回 Err
        let err = http_tts_api_synthesize_stream("default", &make_tts("/json-err"), "你好", 5)
            .await
            .err()
            .expect("JSON 错误体应返回 Err");
        assert!(err.to_string().contains("quota exceeded"), "err={err}");

        // 4. 非 2xx 状态码前置报错
        let err = http_tts_api_synthesize_stream("default", &make_tts("/fail"), "你好", 5)
            .await
            .err()
            .expect("502 应返回 Err");
        assert!(err.to_string().contains("502"), "err={err}");

        server.abort();
    }
}
