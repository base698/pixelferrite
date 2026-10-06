//! OpenAI image editing: configuration from `.env` and the HTTP call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use pf_core::{Pixmap, io};

pub const DEFAULT_MODEL: &str = "gpt-image-2";
const DEFAULT_BASE: &str = "https://api.openai.com/v1";

#[derive(Clone)]
pub struct Config {
    pub key: Option<String>,
    pub model: String,
    pub base: String,
    pub quality: Option<String>,
}

/// Where a `.env` may live: the working directory and its parents, next to
/// the executable (and its parents, which covers `target/release`), and the
/// user's config directory.
pub fn env_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut walk = |start: Option<PathBuf>, depth: usize| {
        let mut dir = start;
        for _ in 0..depth {
            let Some(d) = dir else { break };
            out.push(d.join(".env"));
            dir = d.parent().map(PathBuf::from);
        }
    };
    walk(std::env::current_dir().ok(), 6);
    walk(std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)), 4);
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".config/pixelferrite/.env"));
    }
    out.dedup();
    out
}

fn parse_env(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            let (k, v) = line.split_once('=')?;
            if k.trim().is_empty() || k.trim_start().starts_with('#') {
                return None;
            }
            let v = v.trim();
            let v = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or(""),
                _ => v.split(" #").next().unwrap_or("").trim(),
            };
            Some((k.trim().to_owned(), v.to_owned()))
        })
        .collect()
}

impl Config {
    /// Read settings from the nearest `.env` that has each one, falling back
    /// to the environment. The file wins so that a stale key exported by a
    /// shell profile can't shadow the one written for this app. Read fresh
    /// each time so a key added while the app is open is picked up.
    pub fn load() -> Self {
        let files: Vec<HashMap<String, String>> =
            env_files().iter().filter_map(|p| std::fs::read_to_string(p).ok()).map(|t| parse_env(&t)).collect();
        let get = |k: &str| {
            let set = |v: &String| !v.trim().is_empty();
            files.iter().find_map(|f| f.get(k).cloned().filter(set)).or_else(|| std::env::var(k).ok().filter(set))
        };
        Self {
            key: get("OPENAI_API_KEY"),
            model: get("OPENAI_IMAGE_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            base: get("OPENAI_BASE_URL").unwrap_or_else(|| DEFAULT_BASE.to_owned()).trim_end_matches('/').to_owned(),
            quality: get("OPENAI_IMAGE_QUALITY"),
        }
    }
}

/// The image size to send for an area of `w` x `h` pixels. Newer models take
/// any size (multiples of 16, aspect between 1:3 and 3:1); older ones only
/// three fixed sizes, so the area is stretched to the closest and back.
pub fn request_size(model: &str, w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let flexible = model.starts_with("gpt-image-2") || model.starts_with("chatgpt-image");
    if !flexible {
        let aspect = w / h;
        return [(1024, 1024), (1536, 1024), (1024, 1536)]
            .into_iter()
            .min_by(|a, b| {
                let d = |s: &(u32, u32)| (s.0 as f32 / s.1 as f32).ln() - aspect.ln();
                d(a).abs().total_cmp(&d(b).abs())
            })
            .unwrap();
    }
    // Small areas are enlarged so the model has detail to work with; big
    // ones are capped to keep requests quick and affordable.
    let long = w.max(h).clamp(1024.0, 2048.0);
    let aspect = (w / h).clamp(1.0 / 3.0, 3.0);
    let (rw, rh) = if aspect >= 1.0 { (long, long / aspect) } else { (long * aspect, long) };
    let snap = |v: f32| ((v / 16.0).round() as u32 * 16).clamp(256, 3840);
    (snap(rw), snap(rh))
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = format!("pixelferrite-{:016x}", pf_core::document::next_id().wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let mut body = Vec::new();
    for (name, file, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes());
        if let Some(f) = file {
            body.extend_from_slice(format!("; filename=\"{f}\"\r\nContent-Type: image/png").as_bytes());
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Ask the model to edit `image` according to `prompt`. `mask`, if given, is
/// transparent where it should edit. Blocks until the answer arrives.
pub fn edit(cfg: &Config, prompt: &str, image: &Pixmap, mask: Option<&Pixmap>) -> Result<Pixmap, String> {
    let key = cfg.key.as_deref().ok_or("No OpenAI API key found. Put OPENAI_API_KEY=... in a .env file.")?;
    let png = io::encode_png(image).map_err(|e| e.to_string())?;
    let mask_png = mask.map(io::encode_png).transpose().map_err(|e| e.to_string())?;
    let size = format!("{}x{}", image.w, image.h);
    let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![
        ("model", None, cfg.model.as_bytes()),
        ("prompt", None, prompt.as_bytes()),
        ("size", None, size.as_bytes()),
        ("image", Some("image.png"), &png),
    ];
    if let Some(m) = &mask_png {
        parts.push(("mask", Some("mask.png"), m));
    }
    if let Some(q) = &cfg.quality {
        parts.push(("quality", None, q.as_bytes()));
    }
    let (content_type, body) = multipart(&parts);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(format!("{}/images/edits", cfg.base))
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", content_type)
        .send(&body[..])
        .map_err(|e| format!("Couldn't reach OpenAI: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(512 << 20).read_to_string().map_err(|e| format!("Couldn't read the reply: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if !(200..300).contains(&status) {
        let msg = json["error"]["message"].as_str().map(str::to_owned).unwrap_or_else(|| text.chars().take(300).collect());
        return Err(format!("OpenAI returned an error ({status}): {msg}"));
    }
    let b64 = json["data"][0]["b64_json"].as_str().ok_or("OpenAI's reply didn't contain an image.")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("Couldn't decode the image: {e}"))?;
    io::decode_image(&bytes).map_err(|e| format!("Couldn't decode the image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn env_parsing() {
        let e = parse_env("# comment\nOPENAI_API_KEY = sk-abc # mine\nexport A=\"quoted value\"\nB='x'\n\nbroken line\n");
        assert_eq!(e["OPENAI_API_KEY"], "sk-abc");
        assert_eq!((e["A"].as_str(), e["B"].as_str(), e.len()), ("quoted value", "x", 3));
    }

    #[test]
    fn sizes() {
        assert_eq!(request_size("gpt-image-2", 1600, 1200), (1600, 1200));
        assert_eq!(request_size("gpt-image-2", 200, 100), (1024, 512));
        assert_eq!(request_size("gpt-image-2", 6000, 4000), (2048, 1360));
        assert_eq!(request_size("gpt-image-2", 100, 2000), (672, 2000));
        assert_eq!(request_size("gpt-image-1", 1600, 1200), (1536, 1024));
        assert_eq!(request_size("gpt-image-1.5", 500, 900), (1024, 1536));
    }

    /// One real request, using the key from `.env`. Costs money, so it only
    /// runs when asked for: `cargo test -p pixelferrite -- --ignored live_edit`.
    #[test]
    #[ignore]
    fn live_edit() {
        let cfg = Config { quality: Some("low".into()), ..Config::load() };
        assert!(cfg.key.is_some(), "no OPENAI_API_KEY in the environment or a .env file");
        // A red disc on white, with the disc's area marked for editing.
        let (w, h) = request_size(&cfg.model, 600, 400);
        let mut img = Pixmap::filled(w, h, [255; 4]);
        let mut mask = Pixmap::filled(w, h, [0, 0, 0, 255]);
        let (cx, cy, r) = (w as i32 / 2, h as i32 / 2, h as i32 / 4);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let d2 = (x - cx).pow(2) + (y - cy).pow(2);
                if d2 < r * r {
                    img.set(x, y, [220, 30, 30, 255]);
                }
                if d2 < (r + 40) * (r + 40) {
                    mask.set(x, y, [0; 4]);
                }
            }
        }
        let out = edit(&cfg, "Replace the red circle with a blue square.", &img, Some(&mask)).unwrap_or_else(|e| panic!("{e}"));
        eprintln!("model {} returned {}x{} for a {w}x{h} request", cfg.model, out.w, out.h);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ai-live.png"), io::encode_png(&out).unwrap()).unwrap();
    }

    /// The whole request/response path against a stand-in server.
    #[test]
    fn edit_against_local_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head) = text.find("\r\n\r\n") {
                    let len: usize = text.to_ascii_lowercase().split("content-length:").nth(1).unwrap().lines().next().unwrap().trim().parse().unwrap();
                    if req.len() >= head + 4 + len {
                        break;
                    }
                }
            }
            let reply = io::encode_png(&Pixmap::filled(64, 48, [10, 200, 30, 255])).unwrap();
            let json = format!("{{\"data\":[{{\"b64_json\":\"{}\"}}]}}", base64::engine::general_purpose::STANDARD.encode(reply));
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        let cfg = Config { key: Some("sk-test".into()), model: "gpt-image-2".into(), base, quality: Some("low".into()) };
        let img = Pixmap::filled(64, 48, [255, 0, 0, 255]);
        let out = edit(&cfg, "make it green", &img, Some(&Pixmap::new(64, 48))).unwrap();
        assert_eq!((out.w, out.h, out.px(5, 5)), (64, 48, [10, 200, 30, 255]));
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /images/edits "));
        assert!(req.to_ascii_lowercase().contains("authorization: bearer sk-test"));
        for field in ["name=\"model\"\r\n\r\ngpt-image-2", "name=\"prompt\"\r\n\r\nmake it green", "name=\"size\"\r\n\r\n64x48", "name=\"quality\"\r\n\r\nlow", "name=\"image\"; filename=\"image.png\"", "name=\"mask\"; filename=\"mask.png\""] {
            assert!(req.contains(field), "request should contain {field}");
        }

        // Errors from the API are passed on in plain words.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let cfg = Config { base: format!("http://{}", listener.local_addr().unwrap()), ..cfg };
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 65536];
            let _ = s.read(&mut buf);
            let json = "{\"error\":{\"message\":\"Incorrect API key provided\"}}";
            let _ = write!(s, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len());
        });
        let err = edit(&cfg, "x", &Pixmap::new(16, 16), None).err().unwrap();
        assert!(err.contains("401") && err.contains("Incorrect API key"), "{err}");
    }
}
