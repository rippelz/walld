//! Silent Steam Community auth from the running Steam client's CEF cookie jar.
//!
//! Reads `~/.local/share/Steam/config/htmlcache/Default/Cookies` (Chromium),
//! decrypts `sessionid` + `steamLoginSecure` with the classic Linux Chrome
//! key (PBKDF2-HMAC-SHA1 / "peanuts"), and POSTs to the same
//! `sharedfiles/subscribe|unsubscribe` endpoints the website uses.
//!
//! Requires Steam to have been logged in at least once (cookies present).
//! Keep the Steam client running so workshop downloads apply after subscribe.

use aes::Aes128;
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use pbkdf2::pbkdf2_hmac;
use sha1::Sha1;
use std::path::{Path, PathBuf};
use std::time::Duration;

type Aes128CbcDec = cbc::Decryptor<Aes128>;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) wallstudio/0.2";
const TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone)]
pub struct SteamSession {
    pub sessionid: String,
    pub steam_login_secure: String,
    pub browserid: Option<String>,
    pub steam_country: Option<String>,
}

impl SteamSession {
    pub fn cookie_header(&self) -> String {
        let mut parts = vec![
            format!("sessionid={}", self.sessionid),
            format!("steamLoginSecure={}", self.steam_login_secure),
        ];
        if let Some(b) = &self.browserid {
            parts.push(format!("browserid={b}"));
        }
        if let Some(c) = &self.steam_country {
            parts.push(format!("steamCountry={c}"));
        }
        parts.join("; ")
    }

    /// JWT from `steamLoginSecure` (`steamid||token`) — works as `access_token`
    /// for WebAPI user methods (same auth WE uses for workshop queries).
    pub fn access_token(&self) -> Option<&str> {
        self.steam_login_secure
            .split_once("||")
            .map(|(_, tok)| tok)
            .filter(|t| !t.is_empty())
    }
}

/// Load a live Steam Community session from the local Steam CEF cookie DB.
pub fn load_session() -> Result<SteamSession, String> {
    let db = steam_cookies_db().ok_or_else(|| {
        "Steam cookie DB not found — is Steam installed and have you logged in once?".to_string()
    })?;
    load_session_from_db(&db)
}

pub fn steam_cookies_db() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let candidates = [
        PathBuf::from(&home).join(".local/share/Steam/config/htmlcache/Default/Cookies"),
        PathBuf::from(&home).join(".steam/steam/config/htmlcache/Default/Cookies"),
        PathBuf::from(&home).join(".steam/debian-installation/config/htmlcache/Default/Cookies"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn load_session_from_db(db: &Path) -> Result<SteamSession, String> {
    // Steam may hold a lock — copy the DB (+ WAL) and read the copy.
    let tmp_dir =
        std::env::temp_dir().join(format!("wallstudio-steam-cookies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let tmp_db = tmp_dir.join("Cookies");
    std::fs::copy(db, &tmp_db).map_err(|e| format!("copy cookie db: {e}"))?;
    for suffix in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{suffix}", db.display()));
        if src.is_file() {
            let _ = std::fs::copy(&src, tmp_dir.join(format!("Cookies{suffix}")));
        }
    }

    let result = (|| {
        let conn =
            rusqlite::Connection::open(&tmp_db).map_err(|e| format!("open cookie db: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT name, encrypted_value FROM cookies
                 WHERE host_key = 'steamcommunity.com'
                   AND name IN ('sessionid', 'steamLoginSecure', 'browserid', 'steamCountry')",
            )
            .map_err(|e| e.to_string())?;

        let mut sessionid = None;
        let mut steam_login_secure = None;
        let mut browserid = None;
        let mut steam_country = None;

        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let enc: Vec<u8> = row.get(1)?;
                Ok((name, enc))
            })
            .map_err(|e| e.to_string())?;

        for row in rows {
            let (name, enc) = row.map_err(|e| e.to_string())?;
            if enc.is_empty() {
                continue;
            }
            let plain = match decrypt_chromium_v10(&enc) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("decrypt cookie {name}: {e}");
                    continue;
                }
            };
            let val = String::from_utf8(plain).map_err(|e| e.to_string())?;
            match name.as_str() {
                "sessionid" => sessionid = Some(val),
                "steamLoginSecure" => steam_login_secure = Some(val),
                "browserid" => browserid = Some(val),
                "steamCountry" => steam_country = Some(val),
                _ => {}
            }
        }

        Ok(SteamSession {
            sessionid: sessionid.ok_or("sessionid cookie missing — open Steam and log in")?,
            steam_login_secure: steam_login_secure
                .ok_or("steamLoginSecure cookie missing — open Steam and log in")?,
            browserid,
            steam_country,
        })
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

/// Chromium Linux cookie encryption: `v10` + AES-128-CBC, key from "peanuts".
fn decrypt_chromium_v10(enc: &[u8]) -> Result<Vec<u8>, String> {
    if enc.len() < 4 || &enc[..3] != b"v10" {
        return Err(format!(
            "unsupported cookie prefix (need v10), got {:?}",
            enc.get(..3)
        ));
    }
    let mut key = [0u8; 16];
    pbkdf2_hmac::<Sha1>(b"peanuts", b"saltysalt", 1, &mut key);
    let iv = [b' '; 16];
    let ct = &enc[3..];
    let decryptor = Aes128CbcDec::new((&key).into(), (&iv).into());
    let mut buf = ct.to_vec();
    let pt = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| format!("AES decrypt: {e}"))?;
    Ok(pt.to_vec())
}

/// Subscribe to a workshop published file (account-level). Steam client downloads if online.
pub fn workshop_subscribe(published_file_id: &str, appid: u32) -> Result<(), String> {
    workshop_set_subscription(published_file_id, appid, true)
}

/// Unsubscribe from a workshop published file (account-level).
pub fn workshop_unsubscribe(published_file_id: &str, appid: u32) -> Result<(), String> {
    workshop_set_subscription(published_file_id, appid, false)
}

fn workshop_set_subscription(id: &str, appid: u32, subscribe: bool) -> Result<(), String> {
    if !id.chars().all(|c| c.is_ascii_digit()) || id.is_empty() {
        return Err("invalid workshop id".into());
    }
    let session = load_session()?;
    let action = if subscribe {
        "subscribe"
    } else {
        "unsubscribe"
    };
    let url = format!("https://steamcommunity.com/sharedfiles/{action}");

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(TIMEOUT)
        .user_agent(USER_AGENT)
        .build();

    let referer = format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}");
    let resp = agent
        .post(&url)
        .set("Cookie", &session.cookie_header())
        .set("Origin", "https://steamcommunity.com")
        .set("Referer", &referer)
        .set("X-Requested-With", "XMLHttpRequest")
        .set("Accept", "application/json, text/plain, */*")
        .send_form(&[
            ("id", id),
            ("appid", &appid.to_string()),
            ("sessionid", &session.sessionid),
        ])
        .map_err(|e| format!("steam {action}: {e}"))?;

    let status = resp.status();
    let body = resp.into_string().map_err(|e| e.to_string())?;
    log::info!("steam {action} id={id} status={status} body={body}");

    if status != 200 {
        return Err(format!("steam {action} HTTP {status}: {body}"));
    }

    // `{"success":1}` or `{"success":9}` (already subscribed) etc.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        let code = v
            .get("success")
            .and_then(|x| x.as_i64().or_else(|| x.as_u64().map(|u| u as i64)))
            .unwrap_or(-1);
        // 1 = ok, 8/9 often mean already in desired state for sub flows.
        if code == 1 || (subscribe && (code == 8 || code == 9)) {
            return Ok(());
        }
        if !subscribe && code == 1 {
            return Ok(());
        }
        // Some responses use success:true
        if v.get("success").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(());
        }
        return Err(format!("steam {action} failed (success={code}): {body}"));
    }

    // Non-JSON but 200 with empty body — treat as ok.
    if body.trim().is_empty() || body.contains("success") {
        return Ok(());
    }
    Err(format!("steam {action} unexpected response: {body}"))
}
