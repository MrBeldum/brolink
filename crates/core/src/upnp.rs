//! Automatic port mapping so a home PC is reachable from the internet.
//!
//! Tries NAT-PMP (RFC 6886) against likely gateways, then UPnP IGD (SSDP +
//! WANIPConnection AddPortMapping). Either one turning the host's UDP port
//! into an actually-forwarded WAN address is what makes a pasted ticket work
//! from another country without Tailscale or a relay.
//!
//! All I/O is best-effort and time-bounded. A router that does not speak these
//! protocols, or a CGNAT, returns `None` and the caller falls through to the
//! relay / Tailscale path.

use crate::config::primary_lan_v4;
use anyhow::{anyhow, bail, Context, Result};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

const SSDP_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const NAT_PMP_PORT: u16 = 5351;
const SEARCH: &[u8] = b"M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: 2\r\n\
ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\
\r\n";

/// How the mapping was obtained, for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingVia {
    NatPmp,
    Upnp,
}

impl MappingVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NatPmp => "NAT-PMP",
            Self::Upnp => "UPnP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    pub external: SocketAddrV4,
    pub via: MappingVia,
}

/// Map `internal_port` on this machine through the home gateway.
///
/// `lifetime_secs` is a hint; some IGDs ignore it and keep the mapping until
/// reboot. Returns the public address the rest of the internet should send to.
pub async fn map_udp_port(internal_port: u16, lifetime_secs: u32) -> Option<PortMapping> {
    let lan = primary_lan_v4()?;
    if let Some(m) = nat_pmp_map(lan, internal_port, lifetime_secs).await {
        return Some(m);
    }
    match upnp_map(lan, internal_port, lifetime_secs).await {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::debug!("UPnP mapping failed: {e:#}");
            None
        }
    }
}

/// Gateways NAT-PMP is worth a 400 ms shot at.
pub fn candidate_gateways(lan: Ipv4Addr) -> Vec<Ipv4Addr> {
    let o = lan.octets();
    let mut out = vec![
        Ipv4Addr::new(o[0], o[1], o[2], 1),
        Ipv4Addr::new(o[0], o[1], o[2], 254),
        Ipv4Addr::new(192, 168, 1, 1),
        Ipv4Addr::new(192, 168, 0, 1),
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(192, 168, 1, 254),
        Ipv4Addr::new(192, 168, 0, 254),
    ];
    out.sort();
    out.dedup();
    out.retain(|g| *g != lan);
    out
}

async fn nat_pmp_map(lan: Ipv4Addr, port: u16, lifetime: u32) -> Option<PortMapping> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .ok()?;
    let _ = sock.set_broadcast(true);
    let req = encode_nat_pmp_map(port, lifetime.max(3600));
    for gw in candidate_gateways(lan) {
        let dest = SocketAddr::V4(SocketAddrV4::new(gw, NAT_PMP_PORT));
        let _ = sock.send_to(&req, dest).await;
    }
    let mut buf = [0u8; 32];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        let Ok(Ok((n, _))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else {
            continue;
        };
        if let Some(ext) = decode_nat_pmp_map(&buf[..n], port) {
            return Some(PortMapping {
                external: ext,
                via: MappingVia::NatPmp,
            });
        }
    }
}

/// NAT-PMP UDP mapping request (12 bytes).
pub fn encode_nat_pmp_map(internal_port: u16, lifetime_secs: u32) -> [u8; 12] {
    let mut r = [0u8; 12];
    r[1] = 1; // UDP map
    r[4..6].copy_from_slice(&internal_port.to_be_bytes());
    r[6..8].copy_from_slice(&internal_port.to_be_bytes());
    r[8..12].copy_from_slice(&lifetime_secs.to_be_bytes());
    r
}

/// Parse a NAT-PMP mapping response. Opcode 129 = UDP map reply.
pub fn decode_nat_pmp_map(buf: &[u8], expected_internal: u16) -> Option<SocketAddrV4> {
    if buf.len() < 16 {
        return None;
    }
    if buf[0] != 0 || buf[1] != 129 {
        return None;
    }
    let result = u16::from_be_bytes([buf[2], buf[3]]);
    if result != 0 {
        return None;
    }
    let internal = u16::from_be_bytes([buf[8], buf[9]]);
    if internal != expected_internal {
        return None;
    }
    let external_port = u16::from_be_bytes([buf[10], buf[11]]);
    // The public IPv4 is not in the map reply; opcode 0 would give it.
    // We still return 0.0.0.0:port so the caller can overlay STUN's IP.
    Some(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, external_port))
}

/// NAT-PMP public-address request (2 bytes).
pub fn encode_nat_pmp_addr() -> [u8; 2] {
    [0, 0]
}

pub fn decode_nat_pmp_addr(buf: &[u8]) -> Option<Ipv4Addr> {
    if buf.len() < 12 {
        return None;
    }
    if buf[0] != 0 || buf[1] != 128 {
        return None;
    }
    if u16::from_be_bytes([buf[2], buf[3]]) != 0 {
        return None;
    }
    Some(Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]))
}

async fn upnp_map(lan: Ipv4Addr, port: u16, lifetime: u32) -> Result<PortMapping> {
    let locations = ssdp_locations().await?;
    if locations.is_empty() {
        bail!("no IGD advertised on the LAN");
    }
    let mut last_err = anyhow!("no usable IGD");
    for loc in locations {
        match map_via_device(&loc, lan, port, lifetime).await {
            Ok(m) => return Ok(m),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn ssdp_locations() -> Result<Vec<String>> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
    let _ = sock.send_to(SEARCH, SocketAddr::V4(SSDP_GROUP)).await;
    let mut buf = [0u8; 2048];
    let mut found = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(900);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(Ok((n, _))) = tokio::time::timeout(left, sock.recv_from(&mut buf)).await else {
            continue;
        };
        if let Some(loc) = parse_ssdp_location(std::str::from_utf8(&buf[..n]).unwrap_or("")) {
            if !found.contains(&loc) {
                found.push(loc);
            }
        }
    }
    Ok(found)
}

/// Pull the first `LOCATION:` header out of an SSDP response.
pub fn parse_ssdp_location(resp: &str) -> Option<String> {
    for line in resp.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("LOCATION:")
            .or_else(|| line.strip_prefix("Location:"))
            .or_else(|| line.strip_prefix("location:"))
        else {
            continue;
        };
        let url = rest.trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            return Some(url.to_string());
        }
    }
    None
}

async fn map_via_device(
    location: &str,
    lan: Ipv4Addr,
    port: u16,
    lifetime: u32,
) -> Result<PortMapping> {
    let xml = http_get(location)
        .await
        .context("fetch device description")?;
    let (ctl, service) = find_wan_control(location, &xml)
        .ok_or_else(|| anyhow!("device at {location} has no WANIP/PPP connection"))?;
    let ip = soap_get_external_ip(&ctl, service)
        .await
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    soap_add_port_mapping(&ctl, service, lan, port, lifetime).await?;
    let external_port = soap_get_mapped_port(&ctl, service, port)
        .await
        .unwrap_or(port);
    if ip.is_unspecified() {
        bail!("IGD mapped the port but did not report an external IP");
    }
    Ok(PortMapping {
        external: SocketAddrV4::new(ip, external_port),
        via: MappingVia::Upnp,
    })
}

/// Locate a WANIPConnection / WANPPPConnection control URL in a device XML.
///
/// Returns `(absolute_url, service_type)`.
pub fn find_wan_control(device_url: &str, xml: &str) -> Option<(String, &'static str)> {
    for (needle, service) in [
        (
            "urn:schemas-upnp-org:service:WANIPConnection:1",
            "urn:schemas-upnp-org:service:WANIPConnection:1",
        ),
        (
            "urn:schemas-upnp-org:service:WANIPConnection:2",
            "urn:schemas-upnp-org:service:WANIPConnection:2",
        ),
        (
            "urn:schemas-upnp-org:service:WANPPPConnection:1",
            "urn:schemas-upnp-org:service:WANPPPConnection:1",
        ),
    ] {
        if let Some(url) = control_url_after(xml, needle) {
            return Some((absolutize(device_url, &url), service));
        }
    }
    None
}

fn control_url_after(xml: &str, service_type: &str) -> Option<String> {
    let start = xml.find(service_type)?;
    let window = xml.get(start..start.saturating_add(2500).min(xml.len()))?;
    let tag = window.find("<controlURL>")? + "<controlURL>".len();
    let end = window[tag..].find("</controlURL>")?;
    let url = window[tag..tag + end].trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

fn absolutize(device_url: &str, control: &str) -> String {
    if control.starts_with("http://") || control.starts_with("https://") {
        return control.to_string();
    }
    let base = device_url
        .rsplit_once('/')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| device_url.to_string());
    if control.starts_with('/') {
        // http://192.168.1.1:1900/rootDesc.xml → http://192.168.1.1:1900/ctl
        if let Some(scheme_end) = base.find("://") {
            let rest = &base[scheme_end + 3..];
            let host = rest.split('/').next().unwrap_or(rest);
            let scheme = &base[..scheme_end];
            return format!("{scheme}://{host}{control}");
        }
    }
    format!("{base}/{}", control.trim_start_matches('/'))
}

async fn soap_add_port_mapping(
    control: &str,
    service: &str,
    lan: Ipv4Addr,
    port: u16,
    lifetime: u32,
) -> Result<()> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<u:AddPortMapping xmlns:u="{service}">
<NewRemoteHost></NewRemoteHost>
<NewExternalPort>{port}</NewExternalPort>
<NewProtocol>UDP</NewProtocol>
<NewInternalPort>{port}</NewInternalPort>
<NewInternalClient>{lan}</NewInternalClient>
<NewEnabled>1</NewEnabled>
<NewPortMappingDescription>BroLink</NewPortMappingDescription>
<NewLeaseDuration>{lifetime}</NewLeaseDuration>
</u:AddPortMapping>
</s:Body>
</s:Envelope>"#
    );
    let action = format!("\"{service}#AddPortMapping\"");
    let resp = http_post(control, &[("SOAPAction", action.as_str())], body.as_bytes()).await?;
    if resp.contains("UPnPError") && !resp.contains("<errorCode>718</errorCode>") {
        // 718 = ConflictInMappingEntry: the mapping we want already exists.
        bail!("AddPortMapping rejected: {}", snippet(&resp, 180));
    }
    Ok(())
}

async fn soap_get_external_ip(control: &str, service: &str) -> Result<Ipv4Addr> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<u:GetExternalIPAddress xmlns:u="{service}">
</u:GetExternalIPAddress>
</s:Body>
</s:Envelope>"#
    );
    let action = format!("\"{service}#GetExternalIPAddress\"");
    let resp = http_post(control, &[("SOAPAction", action.as_str())], body.as_bytes()).await?;
    parse_external_ip(&resp).ok_or_else(|| anyhow!("no NewExternalIPAddress in SOAP reply"))
}

async fn soap_get_mapped_port(control: &str, service: &str, port: u16) -> Result<u16> {
    let body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<u:GetSpecificPortMappingEntry xmlns:u="{service}">
<NewRemoteHost></NewRemoteHost>
<NewExternalPort>{port}</NewExternalPort>
<NewProtocol>UDP</NewProtocol>
</u:GetSpecificPortMappingEntry>
</s:Body>
</s:Envelope>"#
    );
    let action = format!("\"{service}#GetSpecificPortMappingEntry\"");
    let _ = http_post(control, &[("SOAPAction", action.as_str())], body.as_bytes()).await;
    Ok(port)
}

/// Extract the IGD's WAN IPv4 from a GetExternalIPAddress SOAP body.
pub fn parse_external_ip(xml: &str) -> Option<Ipv4Addr> {
    let start = xml.find("<NewExternalIPAddress>")? + "<NewExternalIPAddress>".len();
    let end = xml[start..].find("</NewExternalIPAddress>")?;
    xml[start..start + end].trim().parse().ok()
}

fn snippet(s: &str, n: usize) -> String {
    let t = s.trim();
    match t.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t.to_string(),
    }
}

struct UrlParts {
    host: String,
    port: u16,
    path: String,
}

fn split_http_url(url: &str) -> Result<UrlParts> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// IGD URLs are supported (got {url})"))?;
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = format!("/{}", path.trim_start_matches('/'));
    let (host, port) = if let Some((h, p)) = hostport.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(80))
    } else {
        (hostport.to_string(), 80)
    };
    Ok(UrlParts { host, port, path })
}

async fn http_get(url: &str) -> Result<String> {
    http_exchange("GET", url, &[], b"").await
}

async fn http_post(url: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<String> {
    http_exchange("POST", url, headers, body).await
}

async fn http_exchange(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<String> {
    let parts = split_http_url(url)?;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nUser-Agent: BroLink/1.0\r\n",
        path = parts.path,
        host = parts.host,
        port = parts.port,
    );
    if method == "POST" {
        req.push_str("Content-Type: text/xml; charset=\"utf-8\"\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (k, v) in headers {
        req.push_str(k);
        req.push_str(": ");
        req.push_str(v);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    let addr = format!("{}:{}", parts.host, parts.port);
    let mut stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow!("connect timeout {addr}"))?
        .with_context(|| format!("connect {addr}"))?;
    stream.write_all(req.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut out))
        .await
        .map_err(|_| anyhow!("read timeout {addr}"))??;
    let text = String::from_utf8_lossy(&out);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or(&text);
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nat_pmp_map_request_is_well_formed() {
        let r = encode_nat_pmp_map(47850, 3600);
        assert_eq!(r[0], 0);
        assert_eq!(r[1], 1);
        assert_eq!(&r[4..6], &47850u16.to_be_bytes());
        assert_eq!(&r[8..12], &3600u32.to_be_bytes());
    }

    #[test]
    fn nat_pmp_map_reply_roundtrip() {
        let mut buf = [0u8; 16];
        buf[1] = 129;
        buf[8..10].copy_from_slice(&47850u16.to_be_bytes());
        buf[10..12].copy_from_slice(&40000u16.to_be_bytes());
        let addr = decode_nat_pmp_map(&buf, 47850).unwrap();
        assert_eq!(addr.port(), 40000);
        assert!(decode_nat_pmp_map(&buf, 1).is_none());
        buf[2] = 0x00;
        buf[3] = 0x01; // result code 1 = unsupported
        assert!(decode_nat_pmp_map(&buf, 47850).is_none());
        assert!(decode_nat_pmp_map(&[0u8; 8], 47850).is_none());
    }

    #[test]
    fn nat_pmp_addr_reply() {
        let mut buf = [0u8; 12];
        buf[1] = 128;
        buf[8] = 203;
        buf[9] = 0;
        buf[10] = 113;
        buf[11] = 9;
        assert_eq!(
            decode_nat_pmp_addr(&buf).unwrap(),
            Ipv4Addr::new(203, 0, 113, 9)
        );
    }

    #[test]
    fn ssdp_location_is_picked_out_case_insensitively() {
        let r = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nLOCATION: http://192.168.1.1:1900/rootDesc.xml\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
        assert_eq!(
            parse_ssdp_location(r).unwrap(),
            "http://192.168.1.1:1900/rootDesc.xml"
        );
        assert!(parse_ssdp_location("HTTP/1.1 200 OK\r\n\r\n").is_none());
        assert!(parse_ssdp_location("LOCATION: ftp://x").is_none());
    }

    #[test]
    fn control_url_is_resolved_against_the_device_url() {
        let xml = r#"
        <service>
          <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
          <controlURL>/upnp/control/WANIPConn1</controlURL>
        </service>"#;
        let (url, svc) =
            find_wan_control("http://192.168.1.1:1900/rootDesc.xml", xml).expect("control");
        assert_eq!(url, "http://192.168.1.1:1900/upnp/control/WANIPConn1");
        assert!(svc.contains("WANIPConnection"));

        let xml = r#"
        <serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
        <controlURL>http://192.168.0.1:37215/ctl</controlURL>"#;
        let (url, svc) = find_wan_control("http://192.168.0.1:37215/desc", xml).unwrap();
        assert_eq!(url, "http://192.168.0.1:37215/ctl");
        assert!(svc.contains("WANPPP"));
    }

    #[test]
    fn external_ip_parses_out_of_soap() {
        let xml = r#"<s:Envelope><s:Body><NewExternalIPAddress>203.0.113.44</NewExternalIPAddress></s:Body></s:Envelope>"#;
        assert_eq!(
            parse_external_ip(xml).unwrap(),
            Ipv4Addr::new(203, 0, 113, 44)
        );
        assert!(parse_external_ip("<oops/>").is_none());
    }

    #[test]
    fn candidate_gateways_never_include_ourselves() {
        let lan = Ipv4Addr::new(192, 168, 1, 20);
        let gws = candidate_gateways(lan);
        assert!(!gws.contains(&lan));
        assert!(gws.contains(&Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            gws.len(),
            gws.iter().collect::<std::collections::BTreeSet<_>>().len()
        );
    }

    #[test]
    fn http_urls_split_into_host_port_path() {
        let p = split_http_url("http://192.168.1.1:1900/rootDesc.xml").unwrap();
        assert_eq!(p.host, "192.168.1.1");
        assert_eq!(p.port, 1900);
        assert_eq!(p.path, "/rootDesc.xml");
        let p = split_http_url("http://192.168.1.1/").unwrap();
        assert_eq!(p.port, 80);
        assert_eq!(p.path, "/");
        assert!(split_http_url("https://x").is_err());
    }
}
