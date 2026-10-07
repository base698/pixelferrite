//! OpenAI image editing: configuration from `.env` and the HTTP call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use pf_core::{Pixmap, io};

use crate::store::AiSettings;

pub const DEFAULT_MODEL: &str = "gpt-image-2";
const DEFAULT_BASE: &str = "https://api.openai.com/v1";

#[derive(Clone)]
pub struct Config {
    pub key: Option<String>,
    pub model: String,
    pub base: String,
    pub quality: Option<String>,
    /// Where the key was found, for display.
    pub key_source: String,
}

/// What came back from a successful edit.
pub struct Reply {
    pub pixels: Pixmap,
    /// The image file exactly as the API returned it.
    pub file: Vec<u8>,
    pub usage: Option<serde_json::Value>,
}

/// A local .env is only a fallback source of an OpenAI key. Its URL is never
/// trusted, and unrelated ancestors/executable directories are not searched.
pub fn env_files() -> Vec<PathBuf> {
    std::env::current_dir().ok().map(|p| vec![p.join(".env")]).unwrap_or_default()
}

/// Validate the exact upload origin before any bytes or credentials are sent.
pub fn endpoint_origin(base: &str) -> Result<String, String> {
    if base.contains(['#', '@']) || base.trim() != base {
        return Err("The API URL must not contain credentials, fragments or whitespace.".into());
    }
    let uri: ureq::http::Uri = base.parse().map_err(|_| "The API URL is invalid.")?;
    if uri.query().is_some() { return Err("The API URL must not contain a query.".into()); }
    let scheme = uri.scheme_str().ok_or("The API URL must start with https://.")?;
    let host = uri.host().ok_or("The API URL needs a host.")?.to_ascii_lowercase();
    let loopback = host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if scheme != "https" && !(scheme == "http" && loopback) {
        return Err("Use HTTPS for the API URL. HTTP is allowed only for local development on loopback.".into());
    }
    let port = uri.port_u16().unwrap_or(if scheme == "https" { 443 } else { 80 });
    let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
    Ok(if default_port { format!("{scheme}://{host}") } else { format!("{scheme}://{host}:{port}") })
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
    /// Credentials and destination come from one trusted configuration source.
    /// Saved settings win over the process environment. A local .env is a
    /// fallback for OpenAI only and cannot redirect a saved or exported key.
    pub fn load(saved: &AiSettings) -> Self {
        let local = env_files().into_iter().find_map(|p| {
            Some((p.clone(), parse_env(&std::fs::read_to_string(&p).ok()?)))
        });
        let environment = ["OPENAI_API_KEY", "OPENAI_BASE_URL", "OPENAI_IMAGE_MODEL", "OPENAI_IMAGE_QUALITY"]
            .into_iter().filter_map(|k| Some((k.to_owned(), std::env::var(k).ok()?))).collect();
        Self::resolve(saved, &environment, local.as_ref().map(|(p, f)| (p.display().to_string(), f)))
    }

    fn resolve(saved: &AiSettings, environment: &HashMap<String, String>, local: Option<(String, &HashMap<String, String>)>) -> Self {
        let present = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_owned());
        let value = |map: &HashMap<String, String>, k: &str| map.get(k).and_then(|s| present(s));
        let mut cfg = Self {
            key: None, model: present(&saved.model).unwrap_or_else(|| DEFAULT_MODEL.into()),
            base: DEFAULT_BASE.into(), quality: present(&saved.quality).filter(|q| q != "auto"), key_source: String::new(),
        };
        if let Some(key) = present(&saved.api_key) {
            cfg.base = present(&saved.base_url).unwrap_or_else(|| DEFAULT_BASE.into()).trim_end_matches('/').into();
            let bound = present(&saved.api_key_origin).unwrap_or_else(|| "https://api.openai.com".into());
            if endpoint_origin(&cfg.base).ok().as_deref() == Some(bound.as_str()) {
                cfg.key = Some(key);
                cfg.key_source = "Pixelferrite settings".into();
            } else {
                cfg.key_source = "The endpoint changed. Re-enter its key in Settings to authorize this destination.".into();
            }
        } else if let Some(key) = value(environment, "OPENAI_API_KEY") {
            cfg.key = Some(key);
            cfg.key_source = "the OPENAI_API_KEY environment variable".into();
            cfg.base = value(environment, "OPENAI_BASE_URL").unwrap_or_else(|| DEFAULT_BASE.into()).trim_end_matches('/').into();
            cfg.model = value(environment, "OPENAI_IMAGE_MODEL").unwrap_or(cfg.model);
            cfg.quality = value(environment, "OPENAI_IMAGE_QUALITY").or(cfg.quality).filter(|q| q != "auto");
        } else if let Some((path, map)) = local {
            cfg.key = value(map, "OPENAI_API_KEY");
            cfg.key_source = path;
            cfg.model = value(map, "OPENAI_IMAGE_MODEL").unwrap_or(cfg.model);
            cfg.quality = value(map, "OPENAI_IMAGE_QUALITY").or(cfg.quality).filter(|q| q != "auto");
        }
        cfg
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

/// The text actually sent. When only part of the picture is to be replaced
/// the model is told to blend in with the rest and leave it alone; the
/// user's words (possibly none, meaning "just extend the picture") follow.
pub fn full_prompt(user: &str, masked: bool) -> String {
    let user = user.trim();
    if !masked {
        return user.to_owned();
    }
    let rules = "Fill in only the masked (transparent) area of the mask. Continue the surrounding picture seamlessly into it: \
                 same art style, colors, lighting and perspective, with lines and objects that reach the edge of the masked area carrying on across it. \
                 Do not change, restyle or move anything outside the masked area. \
                 Any blur or flat color inside the masked area is only a placeholder: paint over all of it with finished picture, right up to the edges of the image. \
                 The result is one continuous picture that fills the whole frame: no black bars, borders, frames, letterboxing or blank areas.";
    if user.is_empty() { format!("{rules} Add nothing new; just extend the existing picture.") } else { format!("{rules} In the masked area: {user}") }
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

/// Ask the model to edit an image according to `prompt`. `png` is the image
/// (`size` pixels) and `mask_png`, if given, is transparent where it should
/// edit. Blocks until the answer arrives.
pub fn edit(cfg: &Config, prompt: &str, size: (u32, u32), png: &[u8], mask_png: Option<&[u8]>) -> Result<Reply, String> {
    endpoint_origin(&cfg.base)?;
    let key = cfg.key.as_deref().ok_or("No OpenAI API key found. Add one in File > Settings, or put OPENAI_API_KEY=... in a .env file.")?;
    let size = format!("{}x{}", size.0, size.1);
    let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![
        ("model", None, cfg.model.as_bytes()),
        ("prompt", None, prompt.as_bytes()),
        ("size", None, size.as_bytes()),
        ("image", Some("image.png"), png),
    ];
    if let Some(m) = mask_png {
        parts.push(("mask", Some("mask.png"), m));
    }
    if let Some(q) = &cfg.quality {
        parts.push(("quality", None, q.as_bytes()));
    }
    let (content_type, body) = multipart(&parts);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let mut resp = agent
        .post(format!("{}/images/edits", cfg.base))
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", content_type)
        .send(&body[..])
        .map_err(|e| format!("Couldn't reach the image service: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| format!("Couldn't read the reply: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if (300..400).contains(&status) {
        return Err("The image service returned a redirect. Redirects are blocked to protect your key and image.".into());
    }
    if !(200..300).contains(&status) {
        let msg = json["error"]["message"].as_str().map(str::to_owned).unwrap_or_else(|| text.chars().take(300).collect());
        return Err(format!("The image service returned an error ({status}): {msg}"));
    }
    let b64 = json["data"][0]["b64_json"].as_str().ok_or("The image service's reply didn't contain an image.")?;
    let file = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("Couldn't decode the image: {e}"))?;
    let pixels = io::decode_image(&file).map_err(|e| format!("Couldn't decode the image: {e}"))?;
    Ok(Reply { pixels, file, usage: json.get("usage").filter(|u| !u.is_null()).cloned() })
}

/// Convenience for callers holding pixels rather than encoded files.
#[cfg(test)]
fn edit_pixels(cfg: &Config, prompt: &str, image: &Pixmap, mask: Option<&Pixmap>) -> Result<Pixmap, String> {
    let png = io::encode_png(image).map_err(|e| e.to_string())?;
    let mask = mask.map(io::encode_png).transpose().map_err(|e| e.to_string())?;
    edit(cfg, prompt, (image.w, image.h), &png, mask.as_deref()).map(|r| r.pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn credentials_stay_with_their_approved_origin() {
        let hostile = parse_env("OPENAI_BASE_URL=https://collector.invalid/v1\nOPENAI_API_KEY=untrusted-local-key");
        let env = parse_env("OPENAI_API_KEY=exported-key\nOPENAI_BASE_URL=https://configured.example/v1");
        let mut saved = AiSettings { api_key: "saved-key".into(), ..Default::default() };
        let cfg = Config::resolve(&saved, &env, Some(("untrusted/.env".into(), &hostile)));
        assert_eq!(cfg.key.as_deref(), Some("saved-key"));
        assert_eq!(cfg.base, DEFAULT_BASE);
        saved.base_url = "https://changed.example/v1".into();
        assert!(Config::resolve(&saved, &env, None).key.is_none(), "a destination edit must not reuse the old secret");
        saved.api_key_origin = endpoint_origin(&saved.base_url).unwrap();
        assert_eq!(Config::resolve(&saved, &env, None).key.as_deref(), Some("saved-key"));
        let cfg = Config::resolve(&AiSettings::default(), &env, Some(("untrusted/.env".into(), &hostile)));
        assert_eq!(cfg.key.as_deref(), Some("exported-key"));
        assert_eq!(cfg.base, "https://configured.example/v1");
        let cfg = Config::resolve(&AiSettings::default(), &HashMap::new(), Some(("untrusted/.env".into(), &hostile)));
        assert_eq!(cfg.base, DEFAULT_BASE, "dotenv cannot choose an upload destination");
        assert!(env_files().len() <= 1, "do not discover keys in unrelated ancestors");
    }

    #[test]
    fn endpoint_validation_requires_tls_outside_loopback() {
        for base in ["https://api.openai.com/v1", "https://example.test:8443/v1", "http://127.0.0.1:1234", "http://[::1]:1234", "http://localhost:1234"] {
            assert!(endpoint_origin(base).is_ok(), "{base}");
        }
        for base in ["http://example.com/v1", "http://localhost.evil.test", "https://user:secret@example.com/v1", "https://example.com/v1?secret=x", "https://example.com/v1#fragment", "file:///tmp/image", "example.com/v1"] {
            assert!(endpoint_origin(base).is_err(), "{base}");
        }
        assert_eq!(endpoint_origin("https://API.OPENAI.COM:443/v1").unwrap(), "https://api.openai.com");
    }

    #[test]
    fn redirects_cannot_forward_uploaded_images() {
        let destination = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let location = format!("http://{}/stolen", destination.local_addr().unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let cfg = Config { key: Some("fixture-key".into()), model: "fixture".into(), base: format!("http://{}", listener.local_addr().unwrap()), quality: None, key_source: "test".into() };
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len: usize = text.split("content-length:").nth(1).unwrap().lines().next().unwrap().trim().parse().unwrap();
                    if request.len() >= end + 4 + len { break; }
                }
            }
            write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let error = edit_pixels(&cfg, "fixture", &Pixmap::filled(2, 2, [255; 4]), None).err().unwrap();
        server.join().unwrap();
        assert!(error.contains("Redirects are blocked"), "{error}");
        assert!(destination.accept().is_err(), "no connection may be made to a redirect destination");
    }

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
        let cfg = Config { quality: Some("low".into()), ..Config::load(&AiSettings::default()) };
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
        let out = edit_pixels(&cfg, "Replace the red circle with a blue square.", &img, Some(&mask)).unwrap_or_else(|e| panic!("{e}"));
        eprintln!("model {} returned {}x{} for a {w}x{h} request", cfg.model, out.w, out.h);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ai-live.png"), io::encode_png(&out).unwrap()).unwrap();
    }

    /// Checks the model really works from the uploaded picture (no mask):
    /// shapes it could not guess from the prompt must survive the edit.
    #[test]
    #[ignore]
    fn live_whole_image() {
        let cfg = Config { quality: Some("low".into()), ..Config::load(&AiSettings::default()) };
        let (w, h) = request_size(&cfg.model, 1024, 768);
        let mut img = Pixmap::filled(w, h, [255; 4]);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                if (60..260).contains(&x) && (500..700).contains(&y) {
                    img.set(x, y, [20, 160, 60, 255]);
                }
                if (x - 820).pow(2) + (y - 160).pow(2) < 90 * 90 {
                    img.set(x, y, [220, 30, 30, 255]);
                }
            }
        }
        let out = edit_pixels(&cfg, "Change the white background to light yellow. Keep every shape exactly where it is.", &img, None).unwrap_or_else(|e| panic!("{e}"));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
        std::fs::write(dir.join("ai-live-whole.png"), io::encode_png(&out).unwrap()).unwrap();
        let (green, red, bg) = (out.px(160, 600), out.px(820, 160), out.px(500, 380));
        eprintln!("green square {green:?}, red disc {red:?}, background {bg:?}");
        assert!(green[1] > green[0] + 40 && green[1] > green[2] + 40, "green square should still be bottom-left: {green:?}");
        assert!(red[0] > red[1] + 80, "red disc should still be top-right: {red:?}");
        assert!(bg[2] < bg[0].saturating_sub(20), "background should have turned yellow: {bg:?}");
    }

    /// Extending a picture into a selected strip: the bar that runs to the
    /// edge of the art must carry on into the masked area, and the art
    /// itself must come back unchanged.
    #[test]
    #[ignore]
    fn live_extend() {
        let cfg = Config { quality: Some("low".into()), ..Config::load(&AiSettings::default()) };
        let (w, h) = (1024u32, 768u32);
        let mut img = Pixmap::filled(w, h, [255; 4]);
        let mut mask = Pixmap::filled(w, h, [0, 0, 0, 255]);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                if x >= 700 {
                    mask.set(x, y, [0; 4]);
                    continue;
                }
                // Orange sky with a thick tan bar rising to the right.
                let on_bar = (y - (600 - x / 4)).abs() < 28;
                img.set(x, y, if on_bar { [150, 125, 80, 255] } else { [235, 140, 40, 255] });
            }
        }
        let sent = full_prompt("", true);
        let out = edit_pixels(&cfg, &sent, &img, Some(&mask)).unwrap_or_else(|e| panic!("{e}"));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/uitest");
        std::fs::write(dir.join("ai-live-extend.png"), io::encode_png(&out).unwrap()).unwrap();
        let white = |p: [u8; 4]| p[0] > 235 && p[1] > 235 && p[2] > 235;
        let filled = (0..h as i32).step_by(8).filter(|y| !white(out.px(900, *y))).count();
        eprintln!("{filled} of {} sampled pixels in the masked strip were filled", h / 8);
        assert!(filled > 80, "the masked strip should be filled in, not left white");
    }

    #[test]
    fn prompts() {
        assert_eq!(full_prompt(" make it Rome ", false), "make it Rome");
        assert!(full_prompt("a ladybug", true).ends_with("In the masked area: a ladybug"));
        assert!(full_prompt("", true).contains("just extend"));
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
        let cfg = Config { key: Some("sk-test".into()), model: "gpt-image-2".into(), base, quality: Some("low".into()), key_source: String::new() };
        let img = Pixmap::filled(64, 48, [255, 0, 0, 255]);
        let out = edit_pixels(&cfg, "make it green", &img, Some(&Pixmap::new(64, 48))).unwrap();
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
        let err = edit_pixels(&cfg, "x", &Pixmap::new(16, 16), None).err().unwrap();
        assert!(err.contains("401") && err.contains("Incorrect API key"), "{err}");
    }
}
