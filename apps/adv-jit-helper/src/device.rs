//! Connect to the paired local device, mount Apple's DDI and serve IOS-JIT-1.
use crate::gdb;
use idevice::{
    RsdService,
    cryptexd::{self, Cryptex1Assets},
    dvt::remote_server::RemoteServerClient,
    lockdown::LockdownClient,
    mobile_image_mounter::ImageMounter,
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, tunnel::connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::TcpStream;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Keep all files in a DDI set at the same upstream revision, including the mirror.
const DDI_REVISION: &str = "6eae353ae694bda1c421d4a3eee5459ae59c99a1";

/// Enables JIT for `pid` and returns the RemotePairing port that answered.
pub async fn enable(
    pid: u32,
    pairing: &[u8],
    ports: &[u16],
    cache: &Path,
    progress: &impl Fn(&str),
) -> Result<u16> {
    progress("connecting");
    let pairing = RpPairingFile::from_bytes(pairing)?;
    // StikJIT's order, which iOS 27 accepts: the version comes from this
    // system, not lockdown; one tunnel checks and mounts the DDI, and the
    // debugger gets a fresh one.
    let (major, minor) = local_version();
    let cryptex = uses_cryptex(major, minor);
    let (port, remote) = find_service(&pairing, ports, progress).await?;
    let mut tunnel = Tunnel::open(remote).await?;
    progress("checkingDdi");
    if !ddi_mounted(&mut tunnel, cryptex).await? {
        let directory = download_ddi(cache, cryptex, progress).await?;
        progress("mounting");
        mount_ddi(&mut tunnel, cryptex, &directory).await?;
    }
    tunnel.adapter.close().await?;
    progress("connecting");
    let remote = pair(&pairing, port).await.map_err(Miss::into_error)?;
    let mut tunnel = Tunnel::open(remote).await?;
    // StikJIT holds the instruments hub open while it debugs.
    let _hub = RemoteServerClient::connect_rsd(&mut tunnel.adapter, &mut tunnel.rsd).await?;
    let mut debugger = gdb::Debugger::connect_rsd(&mut tunnel.adapter, &mut tunnel.rsd).await?;
    gdb::attach(&mut debugger, pid, major >= 26, progress).await?;
    tunnel.adapter.close().await?;
    Ok(port)
}

type Remote = RemotePairingClient<RpPairingSocket<TcpStream>>;

/// This device as LocalDevVPN routes it, the only route here.
const DEVICE: &str = "10.7.0.1";

/// Connecting and answering a handshake are local and take milliseconds.
const PROBE: Duration = Duration::from_secs(3);

/// Why a port gave no session.
enum Miss {
    /// No RemotePairing service answered there.
    Absent(String),
    /// RemotePairing answered and refused this pairing file.
    Rejected(Box<dyn std::error::Error + Send + Sync>),
}

impl Miss {
    fn into_error(self) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            Miss::Absent(why) => why.into(),
            Miss::Rejected(error) => error,
        }
    }
}

/// Finds this device's RemotePairing service (IOS-JIT-2): the launcher's
/// candidates first, then, as Henge does, every open port in the dynamic
/// range. lockdownd always listens, so it tells whether LocalDevVPN routes
/// at all; without a route the search fails at once.
async fn find_service(
    pairing: &RpPairingFile,
    candidates: &[u16],
    progress: &impl Fn(&str),
) -> Result<(u16, Remote)> {
    let mut misses = Vec::new();
    for &port in candidates {
        match pair(pairing, port).await {
            Ok(remote) => return Ok((port, remote)),
            Err(Miss::Rejected(error)) => return Err(error),
            Err(Miss::Absent(why)) => misses.push(why),
        }
    }
    if let Err(why) = connect(crate::LOCKDOWN_PORT).await {
        return Err(format!("LocalDevVPN unreachable: {why}; {}", misses.join("; ")).into());
    }
    progress("scanning");
    // Below the 256 descriptors iOS grants an extension by default.
    for batch in crate::scan_ports(candidates).chunks(128) {
        for port in open_ports(batch).await {
            match pair(pairing, port).await {
                Ok(remote) => return Ok((port, remote)),
                Err(Miss::Rejected(error)) => return Err(error),
                Err(Miss::Absent(why)) => misses.push(why),
            }
        }
    }
    Err(format!("no RemotePairing service found ({})", misses.join("; ")).into())
}

async fn connect(port: u16) -> std::result::Result<TcpStream, String> {
    match tokio::time::timeout(PROBE, TcpStream::connect((DEVICE, port))).await {
        Ok(Ok(socket)) => Ok(socket),
        Ok(Err(error)) => Err(format!("port {port}: {error}")),
        Err(_) => Err(format!("port {port}: timed out")),
    }
}

/// Connects to `port` and verifies the pairing there.
async fn pair(pairing: &RpPairingFile, port: u16) -> std::result::Result<Remote, Miss> {
    let socket = connect(port).await.map_err(Miss::Absent)?;
    let mut remote = RemotePairingClient::new(RpPairingSocket::new(socket), "Hitode");
    match tokio::time::timeout(PROBE, remote.attempt_pair_verify()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => return Err(Miss::Absent(format!("port {port}: {error}"))),
        Err(_) => return Err(Miss::Absent(format!("port {port}: no answer"))),
    }
    let mut pairing = pairing.clone();
    remote
        .validate_pairing(&mut pairing)
        .await
        .map_err(|e| Miss::Rejected(format!("RemotePairing port {port}: {e}").into()))?;
    Ok(remote)
}

/// The ports of `batch` that accept a connection. Closed ports answer at
/// once through LocalDevVPN, so a batch takes milliseconds.
async fn open_ports(batch: &[u16]) -> Vec<u16> {
    let mut probes = tokio::task::JoinSet::new();
    for &port in batch {
        probes.spawn(async move {
            let connect = TcpStream::connect((DEVICE, port));
            matches!(
                tokio::time::timeout(Duration::from_millis(250), connect).await,
                Ok(Ok(_))
            )
            .then_some(port)
        });
    }
    let mut open: Vec<u16> = probes.join_all().await.into_iter().flatten().collect();
    open.sort_unstable();
    open
}

/// A RemotePairing tunnel to this device and its service directory.
struct Tunnel {
    adapter: AdapterHandle,
    rsd: RsdHandshake,
}

impl Tunnel {
    async fn open(mut remote: Remote) -> Result<Self> {
        let listener = remote.create_tcp_listener().await?;
        let tunnel =
            connect_tls_psk_tunnel_native(connect_vpn(listener).await?, remote.encryption_key())
                .await?;
        let rsd_port = tunnel.info.server_rsd_port;
        let client_address = tunnel.info.client_address.parse()?;
        let server_address = tunnel.info.server_address.parse()?;
        let mut adapter = Adapter::new(
            Box::new(tunnel.into_inner()),
            client_address,
            server_address,
        )
        .to_async_handle();
        let rsd = RsdHandshake::new(adapter.connect(rsd_port).await?).await?;
        Ok(Self { adapter, rsd })
    }
}

/// Whether a DDI is mounted. Like StikJIT, a cryptex system also accepts an
/// image the mounter lists.
async fn ddi_mounted(tunnel: &mut Tunnel, cryptex: bool) -> Result<bool> {
    let installed = if cryptex {
        match cryptexd::installed_ddi(&mut tunnel.adapter, &mut tunnel.rsd).await {
            Ok(Some(_)) => return Ok(true),
            other => Some(other),
        }
    } else {
        None
    };
    let listed = async {
        let mut mounter = ImageMounter::connect_rsd(&mut tunnel.adapter, &mut tunnel.rsd).await?;
        Result::Ok(!mounter.copy_devices().await?.is_empty())
    }
    .await;
    match (installed, listed) {
        (_, Ok(listed)) => Ok(listed),
        (Some(Ok(_)), Err(_)) => Ok(false),
        (Some(Err(error)), Err(_)) => Err(error.into()),
        (None, Err(error)) => Err(error),
    }
}

async fn mount_ddi(tunnel: &mut Tunnel, cryptex: bool, directory: &Path) -> Result<()> {
    if cryptex {
        let assets = Cryptex1Assets::load(directory).await?;
        cryptexd::install_ddi(&mut tunnel.adapter, &mut tunnel.rsd, &assets).await?;
        return Ok(());
    }
    // Personalization needs the chip identifier, which only lockdown reports.
    let mut lockdown = LockdownClient::connect_rsd(&mut tunnel.adapter, &mut tunnel.rsd).await?;
    let chip = lockdown
        .get_value(Some("UniqueChipID"), None)
        .await?
        .as_unsigned_integer()
        .ok_or("missing device chip identifier")?;
    let mut mounter = ImageMounter::connect_rsd(&mut tunnel.adapter, &mut tunnel.rsd).await?;
    mounter
        .mount_personalized_rsd(
            &mut tunnel.adapter,
            &mut tunnel.rsd,
            std::fs::read(directory.join("Image.dmg"))?,
            std::fs::read(directory.join("Image.dmg.trustcache"))?,
            &std::fs::read(directory.join("BuildManifest.plist"))?,
            None,
            chip,
        )
        .await?;
    Ok(())
}

/// Connects to this device through LocalDevVPN, which is the only route here.
async fn connect_vpn(port: u16) -> Result<TcpStream> {
    match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((DEVICE, port))).await {
        Ok(Ok(socket)) => Ok(socket),
        Ok(Err(error)) => Err(format!("LocalDevVPN unreachable: {error} (port {port})").into()),
        Err(_) => {
            Err(format!("LocalDevVPN unreachable: connection timed out (port {port})").into())
        }
    }
}

/// iOS 26.4 and later install the DDI as a cryptex.
fn uses_cryptex(major: u32, minor: u32) -> bool {
    (major, minor) >= (26, 4)
}

/// The DDI set this device needs, judged from the running system; the helper
/// runs on the device it mounts the image on.
pub fn local_cryptex() -> bool {
    let (major, minor) = local_version();
    uses_cryptex(major, minor)
}

fn local_version() -> (u32, u32) {
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    (version.majorVersion as u32, version.minorVersion as u32)
}

fn ddi_variant(cryptex: bool) -> (&'static str, Vec<&'static str>) {
    let mut files = vec!["BuildManifest.plist", "Image.dmg", "Image.dmg.trustcache"];
    if cryptex {
        files.extend(["Image.dmg.cryptex_info", "Image.dmg.root_hash"]);
        ("Xcode_iOS_DDI_Cryptex", files)
    } else {
        ("Xcode_iOS_DDI_Personalized", files)
    }
}

/// Whether every file of the DDI set is already downloaded.
pub fn ddi_present(cache: &Path, cryptex: bool) -> bool {
    let (variant, files) = ddi_variant(cryptex);
    let directory = cache.join(DDI_REVISION).join(variant);
    files.iter().all(|file| directory.join(file).is_file())
}

/// Downloads the missing files of the DDI set. It needs no tunnel, so users
/// can fetch it before LocalDevVPN takes the VPN slot from their usual one.
pub async fn download_ddi(
    cache: &Path,
    cryptex: bool,
    progress: &impl Fn(&str),
) -> Result<PathBuf> {
    let (variant, files) = ddi_variant(cryptex);
    let directory = cache.join(DDI_REVISION).join(variant);
    std::fs::create_dir_all(&directory)?;
    // Image.dmg is about 16 MB; slow links need minutes, not seconds.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()?;
    let mut use_mirror = false;
    for file in files {
        let path = directory.join(file);
        if path.exists() {
            continue;
        }
        progress("downloading");
        let suffix = format!("{DDI_REVISION}/PersonalizedImages/{variant}/{file}");
        let primary =
            format!("https://raw.githubusercontent.com/doronz88/DeveloperDiskImage/{suffix}");
        let mirror = format!("https://cdn.jsdelivr.net/gh/doronz88/DeveloperDiskImage@{suffix}");
        let bytes = if use_mirror {
            download(&client, &mirror).await?
        } else {
            match download(&client, &primary).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    use_mirror = true;
                    download(&client, &mirror).await?
                }
            }
        };
        let temporary = path.with_extension("download");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(temporary, path)?;
    }
    Ok(directory)
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}
