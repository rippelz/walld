//! Steam Workshop browser for Wallpaper Engine (appid 431960).
//!
//! Browse uses the same logged-in Steam session as subscribe/unsubscribe:
//! `IPublishedFileService/QueryFiles` with the JWT from `steamLoginSecure`
//! (identical path the WE client uses — 50 items/page, full catalog).
//!
//! Falls back to public HTML scrape only if Steam cookies are missing.
//! Downloads still land via the Steam client after a silent subscribe.

use crate::steam_session;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wallengine_we::workshop_dir;

/// Wallpaper Engine Steam app id.
pub const WE_APPID: u32 = 431960;

/// Same page size the WE / Steam UGC client typically requests.
pub const WORKSHOP_PAGE_SIZE: u32 = 50;

const USER_AGENT: &str = "wallstudio/0.2 (Wallpaper Engine client for Hyprland)";
const BROWSE_TIMEOUT: Duration = Duration::from_secs(25);
const DETAILS_TIMEOUT: Duration = Duration::from_secs(25);
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkshopSort {
    #[default]
    Trend,
    MostRecent,
    MostSubscribed,
    TextSearch,
}

impl WorkshopSort {
    pub const ALL: [WorkshopSort; 4] = [
        WorkshopSort::Trend,
        WorkshopSort::MostRecent,
        WorkshopSort::MostSubscribed,
        WorkshopSort::TextSearch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Trend => "Trending",
            Self::MostRecent => "Most recent",
            Self::MostSubscribed => "Most subscribed",
            Self::TextSearch => "Relevance",
        }
    }

    fn browsesort(self, has_search: bool) -> &'static str {
        // Free-text search ranks better with Steam's textsearch sort.
        if has_search && !matches!(self, Self::MostRecent | Self::MostSubscribed) {
            return "textsearch";
        }
        match self {
            Self::Trend => "trend",
            Self::MostRecent => "mostrecent",
            Self::MostSubscribed => "totaluniquesubscribers",
            Self::TextSearch => "textsearch",
        }
    }

    /// `EPublishedFileQueryType` for IPublishedFileService/QueryFiles.
    fn query_type(self, has_search: bool) -> u32 {
        if has_search && !matches!(self, Self::MostRecent | Self::MostSubscribed) {
            return 12; // RankedByTextSearch
        }
        match self {
            Self::Trend => 3,          // RankedByTrend
            Self::MostRecent => 1,     // RankedByPublicationDate
            Self::MostSubscribed => 9, // RankedByTotalUniqueSubscriptions
            Self::TextSearch => 12,    // RankedByTextSearch
        }
    }
}

impl std::fmt::Display for WorkshopSort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct WorkshopQuery {
    pub sort: WorkshopSort,
    /// 1-based page index (QueryFiles `page`, 50 items each).
    pub page: u32,
    pub search: String,
    /// Steam `requiredtags[]` values (AND). Type / genre / rating.
    pub tags: Vec<String>,
}

impl Default for WorkshopQuery {
    fn default() -> Self {
        Self {
            sort: WorkshopSort::Trend,
            page: 1,
            search: String::new(),
            tags: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkshopItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub preview_url: String,
    pub subscriptions: u64,
    pub favorited: u64,
    pub views: u64,
    pub file_size: u64,
    pub tags: Vec<String>,
    pub time_created: u64,
    pub time_updated: u64,
    /// Local path to cached preview image, if downloaded.
    pub preview_path: Option<PathBuf>,
}

impl WorkshopItem {
    pub fn media_type_label(&self) -> &'static str {
        for t in &self.tags {
            match t.as_str() {
                "Scene" => return "Scene",
                "Video" => return "Video",
                "Web" => return "Web",
                "Application" => return "App",
                _ => {}
            }
        }
        "Wallpaper"
    }

    pub fn content_rating(&self) -> &str {
        for t in &self.tags {
            if matches!(t.as_str(), "Everyone" | "Questionable" | "Mature") {
                return t.as_str();
            }
        }
        "Everyone"
    }

    pub fn genre_tags(&self) -> Vec<&str> {
        const SKIP: &[&str] = &[
            "Wallpaper",
            "Scene",
            "Video",
            "Web",
            "Application",
            "Everyone",
            "Questionable",
            "Mature",
            "Customizable",
            "Audio responsive",
            "Puppet Warp",
            "Official",
            "Other resolution",
            "Dynamic resolution",
        ];
        self.tags
            .iter()
            .map(|s| s.as_str())
            .filter(|t| {
                !SKIP.iter().any(|s| s.eq_ignore_ascii_case(t))
                    && !t.contains(" x ") // "1920 x 1080"
                    && !t.starts_with("Ultrawide")
            })
            .collect()
    }

    pub fn is_subscribed(&self) -> bool {
        is_subscribed(&self.id)
    }

    pub fn workshop_url(&self) -> String {
        format!(
            "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
            self.id
        )
    }
}

#[derive(Debug, Clone)]
pub struct BrowsePage {
    pub items: Vec<WorkshopItem>,
    pub page: u32,
    pub query: WorkshopQuery,
}

/// True when Steam has downloaded the package under the local workshop root.
pub fn is_subscribed(id: &str) -> bool {
    let dir = workshop_dir().join(id);
    dir.is_dir() && dir.join("project.json").is_file()
}

/// How long to give the Steam client to sync its workshop bookkeeping.
const ACF_SYNC_TIMEOUT: Duration = Duration::from_secs(6);
const ACF_POLL: Duration = Duration::from_millis(200);

/// Steam's install bookkeeping for Wallpaper Engine workshop items.
pub(crate) fn appworkshop_acf() -> PathBuf {
    // .../steamapps/workshop/content/431960 → .../steamapps/workshop
    workshop_dir()
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new(""))
        .join(format!("appworkshop_{WE_APPID}.acf"))
}

/// True when `appworkshop_431960.acf` still lists the item under
/// `WorkshopItemsInstalled` — i.e. Steam believes the files are on disk.
pub fn steam_thinks_installed(id: &str) -> bool {
    let Ok(txt) = std::fs::read_to_string(appworkshop_acf()) else {
        return false;
    };
    acf_block(&txt, "WorkshopItemsInstalled").is_some_and(|b| b.contains(&format!("\"{id}\"")))
}

/// Steam claims the item is installed but the package is gone from disk.
///
/// Subscribing to an item in this state is a silent no-op: Steam accepts the
/// subscription, sees a matching manifest in its own bookkeeping, emits no
/// workshop change, and never downloads anything. The item stays invisible to
/// [`is_subscribed`] forever.
pub fn is_orphaned(id: &str) -> bool {
    steam_thinks_installed(id) && !is_subscribed(id)
}

/// Extract one brace-delimited `"name" { ... }` block from a VDF/ACF document.
pub(crate) fn acf_block<'a>(txt: &'a str, name: &str) -> Option<&'a str> {
    let start = txt.find(&format!("\"{name}\""))?;
    let open = txt[start..].find('{')? + start;
    let mut depth = 0usize;
    for (i, c) in txt[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&txt[open..open + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Block until Steam drops the item from its install records. False on timeout
/// (Steam offline or busy) — callers must then leave the local files alone.
fn wait_until_forgotten(id: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !steam_thinks_installed(id) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(ACF_POLL);
    }
}

/// True when the Steam client is running (needed to act on sub/unsub events).
pub fn steam_running() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let pid = std::fs::read_to_string(format!("{home}/.steam/steam.pid")).ok();
    pid.and_then(|p| p.trim().parse::<u32>().ok())
        .is_some_and(|p| Path::new(&format!("/proc/{p}")).exists())
}

/// Open the workshop item in the Steam client (optional; not used for sub/unsub).
pub fn open_in_steam(id: &str) -> Result<(), String> {
    let uri = format!("steam://url/CommunityFilePage/{id}");
    if std::process::Command::new("steam")
        .arg(&uri)
        .spawn()
        .is_ok()
    {
        return Ok(());
    }
    std::process::Command::new("xdg-open")
        .arg(&uri)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open Steam: {e}"))
}

/// Open the public workshop page in a browser.
pub fn open_in_browser(id: &str) -> Result<(), String> {
    let url = format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}");
    std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open browser: {e}"))
}

/// Account-level subscribe via Steam Community (silent). Steam downloads if online.
///
/// Repairs [`is_orphaned`] items first: Steam only downloads on a *change* to
/// the subscription list, so an item it already believes it has installed needs
/// an unsubscribe round trip to clear the stale manifest record.
pub fn subscribe(id: &str) -> Result<(), String> {
    if is_orphaned(id) {
        log::warn!("workshop {id}: Steam still records a stale install — resyncing");
        steam_session::workshop_unsubscribe(id, WE_APPID)?;
        if !wait_until_forgotten(id, ACF_SYNC_TIMEOUT) {
            // Subscribe anyway so the account state is right, but say plainly
            // that the download will not start until Steam catches up.
            steam_session::workshop_subscribe(id, WE_APPID)?;
            return Err(if steam_running() {
                format!("subscribed, but Steam is still clearing a stale install record for {id} — retry in a moment")
            } else {
                format!("subscribed, but Steam isn't running — start Steam to download {id}")
            });
        }
    }
    steam_session::workshop_subscribe(id, WE_APPID)
}

/// Account-level unsubscribe via Steam Community (silent).
///
/// Returns whether Steam acknowledged and dropped its install record, which is
/// the signal that the local package may be swept.
pub fn unsubscribe(id: &str) -> Result<bool, String> {
    steam_session::workshop_unsubscribe(id, WE_APPID)?;
    Ok(wait_until_forgotten(id, ACF_SYNC_TIMEOUT))
}

/// Delete the local workshop package directory if present.
///
/// Refuses while Steam still tracks the item: removing files behind Steam's
/// back strands the manifest record in `appworkshop_431960.acf` and a later
/// subscribe then silently downloads nothing (see [`is_orphaned`]).
pub fn remove_local(id: &str) -> Result<bool, String> {
    if steam_thinks_installed(id) {
        return Err(format!(
            "Steam still tracks {id} as installed — unsubscribe first, or Steam won't re-download it later"
        ));
    }
    let dir = workshop_dir().join(id);
    if !dir.is_dir() {
        return Ok(false);
    }
    // Safety: only touch numeric workshop ids under the WE content root.
    if !id.chars().all(|c| c.is_ascii_digit()) || id.is_empty() {
        return Err("refusing to remove non-numeric workshop id".into());
    }
    let root = workshop_dir();
    let canonical = dir.canonicalize().map_err(|e| e.to_string())?;
    let root_can = root.canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(&root_can) {
        return Err("workshop path escaped content root".into());
    }
    std::fs::remove_dir_all(&canonical).map_err(|e| format!("remove local: {e}"))?;
    Ok(true)
}

pub fn preview_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("wallengine/workshop-previews");
        }
    }
    PathBuf::from(home).join(".cache/wallengine/workshop-previews")
}

/// Fetch workshop items (**metadata only** — previews filled async).
///
/// Prefers authenticated `QueryFiles` (same session as sub/unsub). Falls back
/// to HTML scrape + GetPublishedFileDetails when Steam isn't logged in.
pub fn browse(query: WorkshopQuery) -> Result<BrowsePage, String> {
    let page = query.page.max(1);
    let mut q = query;
    q.page = page;

    let mut items = match browse_queryfiles(&q) {
        Ok(items) => {
            log::info!("workshop QueryFiles page {} → {} items", page, items.len());
            items
        }
        Err(e) => {
            log::warn!("workshop QueryFiles failed ({e}); falling back to HTML scrape");
            browse_html_fallback(&q)?
        }
    };

    for item in &mut items {
        if let Some(path) = cached_preview_path(&item.id) {
            item.preview_path = Some(path);
        }
    }

    Ok(BrowsePage {
        items,
        page,
        query: q,
    })
}

/// Same API the Wallpaper Engine client uses for Discover / search.
fn browse_queryfiles(query: &WorkshopQuery) -> Result<Vec<WorkshopItem>, String> {
    let session = steam_session::load_session()?;
    let token = session
        .access_token()
        .ok_or("steamLoginSecure missing JWT — open Steam and log in")?
        .to_string();

    let has_search = !query.search.trim().is_empty();
    let query_type = query.sort.query_type(has_search);

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(BROWSE_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();

    // Build query string carefully: requiredtags[i] must be indexed.
    let mut url = format!(
        "https://api.steampowered.com/IPublishedFileService/QueryFiles/v1/\
         ?access_token={}&query_type={}&page={}&numperpage={WORKSHOP_PAGE_SIZE}\
         &appid={WE_APPID}&return_previews=1&return_tags=1\
         &return_short_description=1&strip_description_bbcode=1",
        urlencode(&token),
        query_type,
        query.page.max(1),
    );
    if query_type == 3 {
        url.push_str("&days=7");
    }
    if has_search {
        url.push_str("&search_text=");
        url.push_str(&urlencode(query.search.trim()));
    }
    for (i, tag) in query.tags.iter().filter(|t| !t.is_empty()).enumerate() {
        url.push_str(&format!("&requiredtags%5B{i}%5D="));
        url.push_str(&urlencode(tag));
    }

    log::info!(
        "workshop QueryFiles type={query_type} page={} tags={:?}",
        query.page,
        query.tags
    );

    let resp: QueryFilesResponse = agent
        .get(&url)
        .call()
        .map_err(|e| format!("QueryFiles: {e}"))?
        .into_json()
        .map_err(|e| format!("QueryFiles json: {e}"))?;

    let details = resp.response.publishedfiledetails;
    if details.is_empty() && resp.response.total.unwrap_or(0) == 0 {
        // Empty is ok; distinguish auth death (total missing + empty) vs no hits.
        log::debug!("QueryFiles empty total={:?}", resp.response.total);
    }

    Ok(details
        .into_iter()
        .filter(|d| d.result.unwrap_or(1) == 1 && !d.publishedfileid.is_empty())
        .map(|d| d.into_item())
        .collect())
}

fn browse_html_fallback(query: &WorkshopQuery) -> Result<Vec<WorkshopItem>, String> {
    let ids = scrape_browse_ids(query)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = fetch_details(&ids)?;
    let order: std::collections::HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    items.sort_by_key(|it| order.get(it.id.as_str()).copied().unwrap_or(usize::MAX));
    Ok(items)
}

/// True when a preview file is already on disk for this workshop id.
pub fn cached_preview_path(id: &str) -> Option<PathBuf> {
    let dir = preview_cache_dir();
    for ext in ["gif", "png", "jpg", "jpeg", "webp"] {
        let path = dir.join(format!("{id}.{ext}"));
        if path.is_file() && file_nonempty(&path) {
            if let Some(fixed) = repair_preview_extension(&path) {
                return Some(fixed);
            }
            return Some(path);
        }
    }
    None
}

fn scrape_browse_ids(query: &WorkshopQuery) -> Result<Vec<String>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(BROWSE_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();

    // Fallback: one HTML page (~30 items). Prefer QueryFiles when logged in.
    let url = build_browse_url(query, query.page.max(1));
    log::info!("workshop HTML fallback: {url}");
    let body = agent
        .get(&url)
        .call()
        .map_err(|e| format!("workshop browse request: {e}"))?
        .into_string()
        .map_err(|e| format!("workshop browse body: {e}"))?;

    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for id in regex_lite_ids(&body) {
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Extract published file ids without pulling in the full `regex` crate.
fn regex_lite_ids(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let marker = "filedetails";
    let bytes = html.as_bytes();
    let mut i = 0;
    while i + marker.len() < bytes.len() {
        if &bytes[i..i + marker.len()] == marker.as_bytes() {
            // scan forward for `id=` then digits
            let rest = &html[i..];
            if let Some(pos) = rest.find("id=") {
                let after = &rest[pos + 3..];
                let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                if digits.len() >= 5 && digits.len() <= 20 {
                    out.push(digits);
                }
            }
            i += marker.len();
        } else {
            i += 1;
        }
    }
    out
}

fn build_browse_url(query: &WorkshopQuery, steam_page: u32) -> String {
    let has_search = !query.search.trim().is_empty();
    let sort = query.sort.browsesort(has_search);
    // `numperpage=30` is the public HTML max; WE's native client uses denser
    // QueryFiles pages — we approximate by stacking Steam pages per UI page.
    let mut url = format!(
        "https://steamcommunity.com/workshop/browse/?appid={WE_APPID}\
         &browsesort={sort}&section=readytouseitems&actualsort={sort}\
         &numperpage=30&p={}",
        steam_page.max(1)
    );
    if sort == "trend" {
        url.push_str("&days=7");
    }
    if has_search {
        url.push_str("&searchtext=");
        url.push_str(&urlencode(query.search.trim()));
    }
    for tag in &query.tags {
        if tag.is_empty() {
            continue;
        }
        url.push_str("&requiredtags%5B%5D=");
        url.push_str(&urlencode(tag));
    }
    url
}

/// Count ready workshop packages under the Steam content root (cheap poll).
pub fn count_local_workshop_packages() -> usize {
    let root = workshop_dir();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| e.path().join("project.json").is_file())
        .count()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(nibble(b >> 4));
                out.push(nibble(b & 0xf));
            }
        }
    }
    out
}

fn nibble(n: u8) -> char {
    char::from(if n < 10 { b'0' + n } else { b'A' + (n - 10) })
}

#[derive(Debug, Deserialize)]
struct QueryFilesResponse {
    response: QueryFilesInner,
}

#[derive(Debug, Deserialize)]
struct QueryFilesInner {
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    publishedfiledetails: Vec<QueryFileDetail>,
}

#[derive(Debug, Deserialize)]
struct QueryFileDetail {
    #[serde(default)]
    publishedfileid: String,
    #[serde(default)]
    result: Option<i32>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    short_description: String,
    #[serde(default)]
    file_description: String,
    #[serde(default)]
    preview_url: String,
    #[serde(default)]
    subscriptions: u64,
    #[serde(default)]
    favorited: u64,
    #[serde(default)]
    views: u64,
    #[serde(default)]
    file_size: serde_json::Value,
    #[serde(default)]
    time_created: u64,
    #[serde(default)]
    time_updated: u64,
    #[serde(default)]
    tags: Vec<RawTag>,
}

impl QueryFileDetail {
    fn into_item(self) -> WorkshopItem {
        let file_size = match &self.file_size {
            serde_json::Value::String(s) => s.parse().unwrap_or(0),
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
            _ => 0,
        };
        let description = if !self.short_description.is_empty() {
            self.short_description
        } else {
            self.file_description
        };
        WorkshopItem {
            id: self.publishedfileid,
            title: self.title,
            description,
            preview_url: self.preview_url,
            subscriptions: self.subscriptions,
            favorited: self.favorited,
            views: self.views,
            file_size,
            tags: self.tags.into_iter().map(|t| t.tag).collect(),
            time_created: self.time_created,
            time_updated: self.time_updated,
            preview_path: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DetailsResponse {
    response: DetailsInner,
}

#[derive(Debug, Deserialize)]
struct DetailsInner {
    #[serde(default)]
    publishedfiledetails: Vec<RawDetail>,
}

#[derive(Debug, Deserialize)]
struct RawDetail {
    publishedfileid: String,
    #[serde(default)]
    result: i32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    preview_url: String,
    #[serde(default)]
    subscriptions: u64,
    #[serde(default)]
    favorited: u64,
    #[serde(default)]
    views: u64,
    #[serde(default)]
    file_size: serde_json::Value,
    #[serde(default)]
    time_created: u64,
    #[serde(default)]
    time_updated: u64,
    #[serde(default)]
    tags: Vec<RawTag>,
}

#[derive(Debug, Deserialize)]
struct RawTag {
    tag: String,
}

fn fetch_details(ids: &[String]) -> Result<Vec<WorkshopItem>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(DETAILS_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();

    // Steam accepts batch POSTs; chunk if huge.
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(50) {
        let mut form = vec![("itemcount".to_string(), chunk.len().to_string())];
        for (i, id) in chunk.iter().enumerate() {
            form.push((format!("publishedfileids[{i}]"), id.clone()));
        }
        let body: DetailsResponse = agent
            .post("https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/")
            .send_form(
                &form
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| format!("workshop details request: {e}"))?
            .into_json()
            .map_err(|e| format!("workshop details json: {e}"))?;

        for d in body.response.publishedfiledetails {
            if d.result != 1 && d.result != 0 {
                // result 1 = OK; some payloads omit/zero it but still have title.
                if d.title.is_empty() {
                    continue;
                }
            }
            let file_size = match &d.file_size {
                serde_json::Value::String(s) => s.parse().unwrap_or(0),
                serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
                _ => 0,
            };
            out.push(WorkshopItem {
                id: d.publishedfileid,
                title: d.title,
                description: d.description,
                preview_url: d.preview_url,
                subscriptions: d.subscriptions,
                favorited: d.favorited,
                views: d.views,
                file_size,
                tags: d.tags.into_iter().map(|t| t.tag).collect(),
                time_created: d.time_created,
                time_updated: d.time_updated,
                preview_path: None,
            });
        }
    }
    Ok(out)
}

/// Download (or reuse cached) preview image for a workshop id.
///
/// Extension follows content (`.gif` / `.png` / `.jpg`) so wallstudio's GIF
/// animator can play animated Steam previews instead of freezing on frame 0.
pub fn ensure_preview(id: &str, preview_url: &str) -> Result<PathBuf, String> {
    if preview_url.is_empty() {
        return Err("empty preview url".into());
    }
    let dir = preview_cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Prefer a previously cached file with any known extension. Older builds
    // always wrote `.jpg` even for GIF payloads — fix the extension in place.
    for ext in ["gif", "png", "jpg", "jpeg", "webp"] {
        let path = dir.join(format!("{id}.{ext}"));
        if path.is_file() && file_nonempty(&path) {
            if let Some(fixed) = repair_preview_extension(&path) {
                return Ok(fixed);
            }
            return Ok(path);
        }
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(PREVIEW_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();
    let resp = agent
        .get(preview_url)
        .call()
        .map_err(|e| format!("preview download: {e}"))?;
    let content_type = resp
        .header("content-type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(12 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("preview read: {e}"))?;
    if bytes.len() < 32 {
        return Err("preview too small".into());
    }
    let ext = preview_ext_from_bytes(&bytes, &content_type);
    let path = dir.join(format!("{id}.{ext}"));
    let tmp = dir.join(format!("{id}.{ext}.part"));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    // Drop stale wrong-extension caches (e.g. old always-`.jpg` downloads).
    for other in ["gif", "png", "jpg", "jpeg", "webp"] {
        if other == ext {
            continue;
        }
        let stale = dir.join(format!("{id}.{other}"));
        if stale.is_file() {
            let _ = std::fs::remove_file(stale);
        }
    }
    Ok(path)
}

fn preview_ext_from_bytes(bytes: &[u8], content_type: &str) -> &'static str {
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return "gif";
    }
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return "png";
    }
    if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
        return "jpg";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    if content_type.contains("gif") {
        return "gif";
    }
    if content_type.contains("png") {
        return "png";
    }
    if content_type.contains("webp") {
        return "webp";
    }
    "jpg"
}

/// If a cached preview's magic bytes disagree with its extension (legacy
/// always-`.jpg` downloads of GIFs), rename so the GIF animator can pick it up.
fn repair_preview_extension(path: &Path) -> Option<PathBuf> {
    let mut hdr = [0u8; 16];
    let n = std::fs::File::open(path)
        .ok()
        .and_then(|mut f| {
            use std::io::Read;
            f.read(&mut hdr).ok()
        })
        .unwrap_or(0);
    if n < 6 {
        return None;
    }
    let want = preview_ext_from_bytes(&hdr[..n], "");
    let have = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let have_norm = if have == "jpeg" { "jpg" } else { have.as_str() };
    if have_norm == want {
        return None;
    }
    let dest = path.with_extension(want);
    if std::fs::rename(path, &dest).is_ok() {
        log::info!(
            "workshop preview: renamed {} → {}",
            path.display(),
            dest.display()
        );
        Some(dest)
    } else {
        None
    }
}

fn file_nonempty(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 32).unwrap_or(false)
}

use std::io::Read;
