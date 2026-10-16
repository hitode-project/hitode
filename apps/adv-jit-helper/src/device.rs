//! Connect to the paired local device, mount Apple's DDI and serve IOS-JIT-1.
use crate::gdb;
use idevice::{
    RsdService,
    cryptexd::{self, Cryptex1Assets},
    lockdown::LockdownClient,
    mobile_image_mounter::ImageMounter,
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, tunnel::connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    tcp::adapter::Adapter,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::TcpStream;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Keep all files in a DDI set at the same upstream revision, including the mirror.
const DDI_REVISION: &str = "6eae353ae694bda1c421d4a3eee5459ae59c99a1";

pub async fn enable(
    pid: u32,
    pairing: &[u8],
    cache: &Path,
    progress: &impl Fn(&str),
) -> Result<()> {
    progress("connecting");
    let mut pairing = RpPairingFile::from_bytes(pairing)?;
    let socket = TcpStream::connect(("10.7.0.1", 49152)).await?;
    let mut remote = RemotePairingClient::new(RpPairingSocket::new(socket), "Hitode");
    remote.attempt_pair_verify().await?;
    remote.validate_pairing(&mut pairing).await?;
    let port = remote.create_tcp_listener().await?;
    let tunnel = connect_tls_psk_tunnel_native(
        TcpStream::connect(("10.7.0.1", port)).await?,
        remote.encryption_key(),
    )
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
    let mut rsd = RsdHandshake::new(adapter.connect(rsd_port).await?).await?;
    let mut lockdown = LockdownClient::connect_rsd(&mut adapter, &mut rsd).await?;
    let version = lockdown.get_value(Some("ProductVersion"), None).await?;
    let version = version.as_string().ok_or("missing iOS version")?;
    let mut parts = version.split('.');
    let major: u32 = parts.next().ok_or("missing iOS major version")?.parse()?;
    let minor: u32 = parts.next().unwrap_or("0").parse()?;
    let cryptex = (major, minor) >= (26, 4);
    progress("checkingDdi");
    if cryptex {
        if cryptexd::installed_ddi(&mut adapter, &mut rsd)
            .await?
            .is_none()
        {
            let directory = download_ddi(cache, true, progress).await?;
            progress("mounting");
            let assets = Cryptex1Assets::load(directory).await?;
            cryptexd::install_ddi(&mut adapter, &mut rsd, &assets).await?;
        }
    } else {
        let mut mounter = ImageMounter::connect_rsd(&mut adapter, &mut rsd).await?;
        if mounter.copy_devices().await?.is_empty() {
            let chip = lockdown
                .get_value(Some("UniqueChipID"), None)
                .await?
                .as_unsigned_integer()
                .ok_or("missing device chip identifier")?;
            let directory = download_ddi(cache, false, progress).await?;
            progress("mounting");
            mounter
                .mount_personalized_rsd(
                    &mut adapter,
                    &mut rsd,
                    std::fs::read(directory.join("Image.dmg"))?,
                    std::fs::read(directory.join("Image.dmg.trustcache"))?,
                    &std::fs::read(directory.join("BuildManifest.plist"))?,
                    None,
                    chip,
                )
                .await?;
        }
    }
    // Mounting publishes debugproxy through a fresh RSD handshake.
    rsd = RsdHandshake::new(adapter.connect(rsd_port).await?).await?;
    let mut debugger = gdb::Debugger::connect_rsd(&mut adapter, &mut rsd).await?;
    gdb::attach(&mut debugger, pid, major >= 26, progress).await?;
    adapter.close().await?;
    Ok(())
}

async fn download_ddi(cache: &Path, cryptex: bool, progress: &impl Fn(&str)) -> Result<PathBuf> {
    let variant = if cryptex {
        "Xcode_iOS_DDI_Cryptex"
    } else {
        "Xcode_iOS_DDI_Personalized"
    };
    let directory = cache.join(DDI_REVISION).join(variant);
    std::fs::create_dir_all(&directory)?;
    let mut files = vec!["BuildManifest.plist", "Image.dmg", "Image.dmg.trustcache"];
    if cryptex {
        files.extend(["Image.dmg.cryptex_info", "Image.dmg.root_hash"]);
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
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
