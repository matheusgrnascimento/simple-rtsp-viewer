//! RTSP camera discovery on the local network.
//!
//! In two stages: first it tries to open the RTSP port on every address of the
//! network interfaces' subnets (parallel TCP connections with a short timeout);
//! then, only on the addresses that answered, it opens the video paths with the
//! default login through FFmpeg, to confirm it is a camera and read the
//! resolution. Each subnet is limited to a /24 (254 addresses).

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::credentials::Credentials;
use crate::rtsp_url;

/// Timeout for the RTSP port to accept the connection. On a local network a
/// camera answers within a few milliseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(700);
/// Simultaneous port probe connections.
const PORT_SCAN_CONCURRENCY: usize = 128;
/// Cameras tested by FFmpeg at the same time.
const PROBE_CONCURRENCY: usize = 6;
/// FFmpeg network timeout when testing a path, in microseconds.
const PROBE_TIMEOUT_MICROS: &str = "4000000";

/// Video found on an RTSP path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub codec: String,
}

impl std::fmt::Display for VideoInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}×{} {}", self.width, self.height, self.codec)
    }
}

/// Address with the RTSP port open and the result of testing the paths.
#[derive(Debug, Clone)]
pub struct FoundDevice {
    pub ip: Ipv4Addr,
    /// Main stream, or the reason it did not open.
    pub main: Result<VideoInfo, String>,
    /// Substream, if configured and if it opened.
    pub sub: Option<VideoInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStage {
    /// Probing the RTSP port on each address.
    Ports,
    /// Opening the video paths on the addresses that answered.
    Probing,
    Done,
}

/// Scan progress, shared with the UI.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub stage: ScanStage,
    /// Finished items and total of the current stage.
    pub done: usize,
    pub total: usize,
    /// Scanned subnets, like "192.168.50.0/24".
    pub networks: Vec<String>,
    /// Addresses with the port open, in IP order.
    pub found: Vec<FoundDevice>,
}

pub type SharedProgress = Arc<Mutex<ScanProgress>>;

/// What to look for.
#[derive(Debug, Clone)]
pub struct ScanRequest {
    pub port: u16,
    pub main_path: String,
    pub sub_path: Option<String>,
    pub credentials: Option<Credentials>,
}

/// Starts the scan in the background (needs an active tokio runtime) and
/// returns the progress, updated after each address. `on_progress` is called on
/// every update (the UI uses it to repaint).
pub fn start_scan(
    request: ScanRequest,
    on_progress: impl Fn() + Send + Sync + 'static,
) -> SharedProgress {
    let networks = local_networks();
    let own: BTreeSet<Ipv4Addr> = networks.iter().map(|(ip, _)| *ip).collect();
    let hosts: BTreeSet<Ipv4Addr> = networks
        .iter()
        .flat_map(|(ip, prefix)| hosts_in_network(*ip, *prefix))
        .filter(|host| !own.contains(host))
        .collect();

    let progress = Arc::new(Mutex::new(ScanProgress {
        stage: ScanStage::Ports,
        done: 0,
        total: hosts.len(),
        networks: networks
            .iter()
            .map(|(ip, prefix)| network_label(*ip, *prefix))
            .collect(),
        found: Vec::new(),
    }));

    tokio::spawn(run_scan(
        request,
        hosts.into_iter().collect(),
        progress.clone(),
        Arc::new(on_progress),
    ));
    progress
}

/// Tests an RTSP path through FFmpeg and reads the video. Blocking: call it on
/// its own thread (e.g. `spawn_blocking`).
pub fn probe(url: &str) -> Result<VideoInfo, String> {
    let mut options = ffmpeg_next::Dictionary::new();
    options.set("rtsp_transport", "tcp");
    options.set("timeout", PROBE_TIMEOUT_MICROS);

    let input = ffmpeg_next::format::input_with_dictionary(url, options).map_err(describe_error)?;
    let stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| "no video on this path".to_owned())?;
    let parameters = stream.parameters();
    // SAFETY: reads fields of a valid `AVCodecParameters` while `input` is
    // open.
    let (width, height) = unsafe {
        let raw = parameters.as_ptr();
        ((*raw).width, (*raw).height)
    };
    Ok(VideoInfo {
        width: u32::try_from(width).unwrap_or(0),
        height: u32::try_from(height).unwrap_or(0),
        codec: parameters.id().name().to_uppercase(),
    })
}

/// Short message for the most common errors when opening an RTSP path.
fn describe_error(err: ffmpeg_next::Error) -> String {
    match err {
        ffmpeg_next::Error::HttpUnauthorized | ffmpeg_next::Error::HttpForbidden => {
            "login refused".to_owned()
        }
        ffmpeg_next::Error::HttpNotFound => "path not found".to_owned(),
        other => other.to_string(),
    }
}

/// Private IPv4 addresses of the network interfaces (no loopback or
/// link-local), with the prefix length of each subnet.
fn local_networks() -> Vec<(Ipv4Addr, u8)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.prefixlen)),
            if_addrs::IfAddr::V6(_) => None,
        })
        .filter(|(ip, _)| ip.is_private() && !ip.is_loopback() && !ip.is_link_local())
        .collect()
}

/// Host addresses of the subnet of `ip`, limited to a /24 (larger subnets are
/// scanned only within the /24 of `ip` itself). Excludes the network and
/// broadcast addresses; empty for /31 and /32.
pub fn hosts_in_network(ip: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    let prefix = prefix.clamp(24, 32);
    if prefix >= 31 {
        return Vec::new();
    }
    let mask = u32::MAX << (32 - prefix);
    let network = u32::from(ip) & mask;
    let broadcast = network | !mask;
    ((network + 1)..broadcast).map(Ipv4Addr::from).collect()
}

fn network_label(ip: Ipv4Addr, prefix: u8) -> String {
    let prefix = prefix.clamp(24, 32);
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    format!("{}/{prefix}", Ipv4Addr::from(u32::from(ip) & mask))
}

async fn run_scan(
    request: ScanRequest,
    hosts: Vec<Ipv4Addr>,
    progress: SharedProgress,
    on_progress: Arc<dyn Fn() + Send + Sync>,
) {
    let open = find_open_ports(&hosts, request.port, &progress, &on_progress).await;

    update(&progress, &on_progress, |progress| {
        progress.stage = ScanStage::Probing;
        progress.done = 0;
        progress.total = open.len();
    });

    let semaphore = Arc::new(Semaphore::new(PROBE_CONCURRENCY));
    let mut probes = JoinSet::new();
    for ip in open {
        let semaphore = semaphore.clone();
        let request = request.clone();
        probes.spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok()?;
            tokio::task::spawn_blocking(move || probe_device(ip, &request))
                .await
                .ok()
        });
    }
    while let Some(result) = probes.join_next().await {
        let device = result.ok().flatten();
        update(&progress, &on_progress, |progress| {
            if let Some(device) = device {
                progress.found.push(device);
                progress.found.sort_by_key(|device| device.ip);
            }
            progress.done += 1;
        });
    }

    update(&progress, &on_progress, |progress| {
        progress.stage = ScanStage::Done;
    });
}

async fn find_open_ports(
    hosts: &[Ipv4Addr],
    port: u16,
    progress: &SharedProgress,
    on_progress: &Arc<dyn Fn() + Send + Sync>,
) -> Vec<Ipv4Addr> {
    let semaphore = Arc::new(Semaphore::new(PORT_SCAN_CONCURRENCY));
    let mut checks = JoinSet::new();
    for &ip in hosts {
        let semaphore = semaphore.clone();
        checks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok()?;
            let connected =
                tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((ip, port)))
                    .await;
            matches!(connected, Ok(Ok(_))).then_some(ip)
        });
    }

    let mut open = Vec::new();
    while let Some(result) = checks.join_next().await {
        if let Ok(Some(ip)) = result {
            open.push(ip);
        }
        update(progress, on_progress, |progress| progress.done += 1);
    }
    open.sort();
    open
}

fn probe_device(ip: Ipv4Addr, request: &ScanRequest) -> FoundDevice {
    let host = ip.to_string();
    let url = |path: &str| rtsp_url::build(&host, request.port, path, request.credentials.as_ref());

    let main = probe(&url(&request.main_path));
    let sub = match (&main, &request.sub_path) {
        (Ok(_), Some(sub_path)) => probe(&url(sub_path)).ok(),
        _ => None,
    };
    FoundDevice { ip, main, sub }
}

fn update(
    progress: &SharedProgress,
    on_progress: &Arc<dyn Fn() + Send + Sync>,
    change: impl FnOnce(&mut ScanProgress),
) {
    change(&mut progress.lock().unwrap_or_else(PoisonError::into_inner));
    on_progress();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_24_has_254_hosts_without_network_and_broadcast() {
        let hosts = hosts_in_network(Ipv4Addr::new(192, 168, 100, 110), 24);
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(192, 168, 100, 1)));
        assert_eq!(hosts.last(), Some(&Ipv4Addr::new(192, 168, 100, 254)));
    }

    #[test]
    fn larger_networks_are_limited_to_the_own_slash_24() {
        let hosts = hosts_in_network(Ipv4Addr::new(10, 1, 2, 3), 16);
        assert_eq!(hosts.len(), 254);
        assert!(hosts.iter().all(|host| host.octets()[..3] == [10, 1, 2]));
        assert_eq!(network_label(Ipv4Addr::new(10, 1, 2, 3), 16), "10.1.2.0/24");
    }

    #[test]
    fn smaller_networks_keep_their_size() {
        let hosts = hosts_in_network(Ipv4Addr::new(192, 168, 1, 70), 26);
        assert_eq!(hosts.len(), 62);
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(192, 168, 1, 65)));
        assert!(hosts_in_network(Ipv4Addr::new(192, 168, 1, 1), 31).is_empty());
        assert!(hosts_in_network(Ipv4Addr::new(192, 168, 1, 1), 32).is_empty());
    }

    #[test]
    fn video_info_is_shown_with_resolution_and_codec() {
        let info = VideoInfo {
            width: 2560,
            height: 1440,
            codec: "H264".to_owned(),
        };
        assert_eq!(info.to_string(), "2560×1440 H264");
    }
}
