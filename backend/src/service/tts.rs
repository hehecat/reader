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
/// FILETIME 偏移：1601-01-01 → 1970-01-01 的 100ns 间隔数
const FILETIME_EPOCH_OFFSET_TICKS: i64 = 116_444_736_000_000_000;
/// WSS 端点
const EDGE_WSS_URL: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
/// 输出音频格式（MP3）
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

/// 生成 Sec-MS-GEC 鉴权 token（.NET FILETIME 小端十六进制；与 edge-tts 算法一致）
/// 返回 (Sec-MS-GEC, Sec-MS-GEC-Version)
pub fn sec_ms_gec_at(unix_secs: i64) -> (String, String) {
    let ticks = unix_secs * 10_000_000 + FILETIME_EPOCH_OFFSET_TICKS;
    let stamp = ticks
        .to_le_bytes()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>();
    // Version：次日日期整数（YYYYMMDD+1）编码为 FILETIME 样式（edge-tts 同款算法）
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap());
    let date_int: i64 = dt.format("%Y%m%d").to_string().parse().unwrap_or(19700101);
    let ticks_v = (date_int + 1) * 10_000_000 + FILETIME_EPOCH_OFFSET_TICKS;
    let stamp_v = ticks_v
        .to_le_bytes()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>();
    (stamp, stamp_v)
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

/// 单块 Edge TTS 合成（一次 WSS 会话）
async fn edge_synthesize_chunk(
    text: &str,
    voice: &str,
    rate: &str,
    pitch: &str,
    volume: &str,
    style: Option<&str>,
) -> Result<Vec<u8>> {
    let connection_id = uuid::Uuid::new_v4();
    let url = edge_wss_url(&connection_id.to_string());

    let request = http::Request::builder()
        .uri(&url)
        .header("Pragma", "no-cache")
        .header("Cache-Control", "no-cache")
        .header(
            "Origin",
            "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold",
        )
        .body(())
        .map_err(|e| anyhow!("构造 WSS 请求失败: {e}"))?;

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

    /// Sec-MS-GEC：已知时刻的确定性输出（16 位大写十六进制）
    #[test]
    fn test_sec_ms_gec() {
        let (stamp, stamp_v) = sec_ms_gec_at(0);
        assert_eq!(stamp.len(), 16);
        assert_eq!(stamp_v.len(), 16);
        assert!(stamp
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        assert!(stamp_v
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        assert_eq!(sec_ms_gec_at(0), sec_ms_gec_at(0), "同一时刻输出应确定");
        // unix=0 → FILETIME 偏移量本体
        assert_eq!(
            stamp,
            116444736000000000i64
                .to_le_bytes()
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>()
        );
        // Version 基于次日日期，应大于 Stamp 数值编码
        assert_ne!(stamp, stamp_v);
        // WSS URL 含鉴权参数
        let url = edge_wss_url("conn-1");
        assert!(url.starts_with(
            "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?"
        ));
        assert!(url.contains("TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4"));
        assert!(url.contains("Sec-MS-GEC=") && url.contains("Sec-MS-GEC-Version="));
        assert!(url.contains("ConnectionId=conn-1"));
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
}
