//! This computer's UltraViewer registration profile. The installation
//! identity and random password persist; network and platform values are
//! refreshed on every load.
//!
//! UltraViewer derives the ID from the hardware address, so outgoing and
//! incoming sessions share one profile (and one password) instead of two
//! registrations that would overwrite each other on the server.

use std::ffi::CStr;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use md5::{Digest, Md5};
use rand::Rng;
use ultraviewer_protocol::{ClientRegistrationRequest, RelayIPv4Address, CLIENT_VERSION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileError {
    NetworkUnavailable,
    NetworkInspectionFailed(i32),
    HardwareAddressUnavailable,
    InvalidStoredProfile,
    InvalidStorageLocation,
    Io(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::NetworkUnavailable => "No active IPv4 network connection is available. Connect this computer to a network and try again.".to_string(),
            Self::NetworkInspectionFailed(_) => "This computer's network interfaces could not be read. Try connecting again.".to_string(),
            Self::HardwareAddressUnavailable => "No hardware address is available for this computer's active network interfaces.".to_string(),
            Self::InvalidStoredProfile => "The saved client profile is invalid. Its installation identity has been preserved on disk.".to_string(),
            Self::InvalidStorageLocation => "The private client profile location is unavailable or is not a regular folder/file.".to_string(),
            Self::Io(message) => format!("The client profile could not be saved: {message}"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for ProfileError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkIdentity {
    pub address: RelayIPv4Address,
    pub mac_address: String,
}

#[derive(Debug, Clone, Default)]
pub struct NetworkInterface {
    pub name: String,
    pub addresses: Vec<RelayIPv4Address>,
    pub mac_address: Option<String>,
}

const MAXIMUM_PROFILE_BYTES: u64 = 65_536;

/// Token from UltraViewer's captcha page. Like the desktop client, it is kept
/// for this run only and sent with every later registration.
static CAPTCHA_TOKEN: Mutex<String> = Mutex::new(String::new());

#[cfg_attr(not(feature = "captcha"), allow(dead_code))]
pub fn set_captcha_token(token: String) {
    *CAPTCHA_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = token;
}

fn captcha_token() -> String {
    CAPTCHA_TOKEN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// What to tell the person when registration waits for the captcha.
pub fn captcha_message(solve: &str) -> String {
    if cfg!(feature = "captcha") {
        crate::i18n::tr(solve)
    } else {
        crate::i18n::tr("UltraViewer asks for a captcha, which this build cannot show. Install the .deb, .rpm or Arch package to verify this computer once.")
    }
}

fn profile_directory() -> PathBuf {
    crate::settings::data_directory()
}

/// Loads (or creates) the profile and refreshes its network/platform values.
pub fn load_or_create() -> Result<ClientRegistrationRequest, ProfileError> {
    let network = discover_network()?;
    let mut request = load_or_create_in(&profile_directory(), network, &host_name(), &user_name(), &kernel_release())?;
    request.captcha_token = captcha_token();
    Ok(request)
}

/// Replaces the random password while keeping the installation identity.
pub fn reset_password() -> Result<ClientRegistrationRequest, ProfileError> {
    let directory = profile_directory();
    let mut request = load_or_create()?;
    request.password = random_password();
    let token = std::mem::take(&mut request.captcha_token);
    write_profile(&directory.join("registration.json"), &request)?;
    request.captcha_token = token;
    Ok(request)
}

pub fn load_or_create_in(directory: &Path, network: NetworkIdentity, host_name: &str, user_name: &str, release: &str)
    -> Result<ClientRegistrationRequest, ProfileError> {
    std::fs::create_dir_all(directory).map_err(|e| ProfileError::Io(e.to_string()))?;
    let metadata = std::fs::symlink_metadata(directory).map_err(|e| ProfileError::Io(e.to_string()))?;
    if !metadata.is_dir() {
        return Err(ProfileError::InvalidStorageLocation);
    }
    let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    let destination = directory.join("registration.json");
    let (identity, password) = match std::fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(ProfileError::InvalidStorageLocation);
            }
            if metadata.len() == 0 || metadata.len() > MAXIMUM_PROFILE_BYTES {
                return Err(ProfileError::InvalidStoredProfile);
            }
            let bytes = std::fs::read(&destination).map_err(|e| ProfileError::Io(e.to_string()))?;
            let stored: ClientRegistrationRequest = serde_json::from_slice(&bytes).map_err(|_| ProfileError::InvalidStoredProfile)?;
            let valid_identity = stored.device_identity.len() == 32
                && stored.device_identity.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
            let valid_password = stored.password.len() == 8 && stored.password.bytes().all(|b| b.is_ascii_digit());
            if !valid_identity || !valid_password {
                return Err(ProfileError::InvalidStoredProfile);
            }
            (stored.device_identity, stored.password)
        }
        Err(_) => {
            let installation: [u8; 16] = rand::thread_rng().gen();
            let identity = Md5::digest(installation).iter().map(|b| format!("{b:02X}")).collect();
            (identity, random_password())
        }
    };
    let request = make_request(identity, password, network, host_name, user_name, release);
    // Check the real codec's field/encoding constraints before persistence.
    request.encode().map_err(|e| ProfileError::Io(e.to_string()))?;
    write_profile(&destination, &request)?;
    Ok(request)
}

fn write_profile(destination: &Path, request: &ClientRegistrationRequest) -> Result<(), ProfileError> {
    let mut bytes = serde_json::to_vec_pretty(request).map_err(|e| ProfileError::Io(e.to_string()))?;
    bytes.push(b'\n');
    let temporary = destination.with_extension("json.tmp");
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| ProfileError::Io(e.to_string()))?;
    file.write_all(&bytes).map_err(|e| ProfileError::Io(e.to_string()))?;
    file.sync_all().map_err(|e| ProfileError::Io(e.to_string()))?;
    std::fs::rename(&temporary, destination).map_err(|e| ProfileError::Io(e.to_string()))?;
    Ok(())
}

fn random_password() -> String {
    let mut rng = rand::thread_rng();
    (0..8).map(|_| char::from(b'0' + rng.gen_range(0..10))).collect()
}

/// Kernel major/minor/patch map to .NET Major/Minor/Build. Revision has no
/// Linux counterpart and remains zero.
pub fn operating_system_wire_value(release: &str) -> String {
    let numbers: Vec<u32> = release
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();
    let major = numbers.first().copied().unwrap_or(0);
    let minor = numbers.get(1).copied().unwrap_or(0);
    let patch = numbers.get(2).copied().unwrap_or(0);
    format!("{patch}|{major}|0|{minor}|0|0")
}

pub fn user_name_wire_value(name: &str) -> String {
    if name.is_ascii() && name.bytes().all(|b| (32..=126).contains(&b) && b != b':') {
        name.to_string()
    } else {
        base64::engine::general_purpose::STANDARD.encode(name.as_bytes())
    }
}

fn make_request(identity: String, password: String, network: NetworkIdentity, host_name: &str, user_name: &str, release: &str)
    -> ClientRegistrationRequest {
    ClientRegistrationRequest {
        device_identity: identity,
        password,
        local_address: network.address,
        computer_name_utf8_base64: base64::engine::general_purpose::STANDARD.encode(host_name.as_bytes()),
        client_version: CLIENT_VERSION.into(),
        mac_address: network.mac_address,
        disk_identity: String::new(),
        operating_system: operating_system_wire_value(release),
        installer_referral: 0,
        custom_password: String::new(),
        uses_proxy: false,
        quick_support_metadata: String::new(),
        user_name_wire_value: user_name_wire_value(user_name),
        active_session_id: 0,
        process_session_id: 0,
        preferred_id_text: "0".into(),
        referral_id_text: "0".into(),
        automatic_update: 0,
        system_folder_creation_ticks: "0".into(),
        program_files_creation_ticks: "0".into(),
        system_volume_information_creation_ticks: "0".into(),
        license_context1: String::new(),
        license_context2: String::new(),
        captcha_token: String::new(),
    }
}

fn usable_address(address: &RelayIPv4Address) -> bool {
    let o = address.octets();
    o[0] != 0 && o[0] != 127 && o[0] < 224 && !(o[0] == 169 && o[1] == 254)
}

pub fn select_network(interfaces: &[NetworkInterface], primary: Option<&str>) -> Result<NetworkIdentity, ProfileError> {
    let mut ordered: Vec<&NetworkInterface> = interfaces.iter().collect();
    ordered.sort_by(|a, b| {
        let a_primary = Some(a.name.as_str()) == primary;
        let b_primary = Some(b.name.as_str()) == primary;
        b_primary.cmp(&a_primary).then_with(|| a.name.cmp(&b.name))
    });
    let (source, address) = ordered
        .iter()
        .find_map(|item| item.addresses.iter().find(|a| usable_address(a)).map(|a| (*item, *a)))
        .ok_or(ProfileError::NetworkUnavailable)?;
    // A VPN's primary IPv4 interface may have no link-layer address. Use an
    // actual active hardware interface's MAC; never synthesize one.
    let hardware = source
        .mac_address
        .clone()
        .or_else(|| ordered.iter().find_map(|i| i.mac_address.clone()))
        .ok_or(ProfileError::HardwareAddressUnavailable)?;
    Ok(NetworkIdentity { address, mac_address: hardware })
}

/// Interface carrying the default IPv4 route, from /proc/net/route.
fn primary_interface() -> Option<String> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.len() > 6 && fields[1] == "00000000" && fields[7] == "00000000").then(|| {
                let metric: u32 = fields[6].parse().unwrap_or(u32::MAX);
                (metric, fields[0].to_string())
            })
        })
        .min()
        .map(|(_, name)| name)
}

pub fn discover_network() -> Result<NetworkIdentity, ProfileError> {
    let mut interfaces: Vec<NetworkInterface> = Vec::new();
    // SAFETY: getifaddrs returns a linked list freed with freeifaddrs below;
    // each sockaddr is read according to its family.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return Err(ProfileError::NetworkInspectionFailed(*libc::__errno_location()));
        }
        let mut cursor = head;
        while !cursor.is_null() {
            let entry = &*cursor;
            cursor = entry.ifa_next;
            let flags = entry.ifa_flags;
            if flags & libc::IFF_UP as u32 == 0 || flags & libc::IFF_RUNNING as u32 == 0 || flags & libc::IFF_LOOPBACK as u32 != 0 {
                continue;
            }
            if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
                continue;
            }
            let name = CStr::from_ptr(entry.ifa_name).to_string_lossy().into_owned();
            let index = match interfaces.iter().position(|i| i.name == name) {
                Some(index) => index,
                None => {
                    interfaces.push(NetworkInterface { name: name.clone(), ..Default::default() });
                    interfaces.len() - 1
                }
            };
            match i32::from((*entry.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let address = &*(entry.ifa_addr as *const libc::sockaddr_in);
                    let octets = address.sin_addr.s_addr.to_ne_bytes();
                    interfaces[index].addresses.push(RelayIPv4Address::from_octets(octets));
                }
                libc::AF_PACKET => {
                    let link = &*(entry.ifa_addr as *const libc::sockaddr_ll);
                    if link.sll_halen == 6 {
                        let bytes = &link.sll_addr[..6];
                        if bytes.iter().any(|&b| b != 0) && bytes[0] & 1 == 0 {
                            interfaces[index].mac_address = Some(bytes.iter().map(|b| format!("{b:02X}")).collect());
                        }
                    }
                }
                _ => {}
            }
        }
        libc::freeifaddrs(head);
    }
    select_network(&interfaces, primary_interface().as_deref())
}

pub fn host_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: buffer is valid for its length; result is NUL-terminated on success.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let name = if ok {
        CStr::from_bytes_until_nul(&buffer).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    if name.is_empty() { "linux".into() } else { name }
}

pub fn user_name() -> String {
    std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "user".into())
}

fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network() -> NetworkIdentity {
        NetworkIdentity { address: RelayIPv4Address::parse("192.168.1.10").unwrap(), mac_address: "001122334455".into() }
    }

    #[test]
    fn profile_round_trip_preserves_identity_and_refreshes_network() {
        let directory = std::env::temp_dir().join(format!("linuxviewer-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let first = load_or_create_in(&directory, network(), "host", "user", "7.2.9-arch1-1").unwrap();
        assert_eq!(first.device_identity.len(), 32);
        assert_eq!(first.password.len(), 8);
        assert_eq!(first.operating_system, "9|7|0|2|0|0");
        let mut moved = network();
        moved.address = RelayIPv4Address::parse("10.0.0.5").unwrap();
        let second = load_or_create_in(&directory, moved, "host", "người", "7.2.9").unwrap();
        assert_eq!(second.device_identity, first.device_identity);
        assert_eq!(second.password, first.password);
        assert_eq!(second.local_address.to_string(), "10.0.0.5");
        assert_eq!(second.user_name_wire_value, "bmfGsOG7nWk=");
        let mode = std::fs::metadata(directory.join("registration.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn network_selection_prefers_primary_and_real_hardware() {
        let interfaces = vec![
            NetworkInterface { name: "tun0".into(), addresses: vec![RelayIPv4Address::parse("10.8.0.2").unwrap()], mac_address: None },
            NetworkInterface { name: "wlan0".into(), addresses: vec![RelayIPv4Address::parse("192.168.1.19").unwrap()], mac_address: Some("AABBCCDDEEFF".into()) },
        ];
        let selected = select_network(&interfaces, Some("tun0")).unwrap();
        assert_eq!(selected.address.to_string(), "10.8.0.2");
        assert_eq!(selected.mac_address, "AABBCCDDEEFF");
        let selected = select_network(&interfaces, Some("wlan0")).unwrap();
        assert_eq!(selected.address.to_string(), "192.168.1.19");
        assert!(select_network(&[], None).is_err());
    }
}
