use crate::{DISCOVERY_PORT, views::NetworkInterfaceView};
use std::{
    net::UdpSocket as StdUdpSocket,
    net::{IpAddr, SocketAddr},
};

pub(crate) fn parse_socket_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|err| format!("invalid socket address: {err}"))
}

pub(crate) fn advertised_addr(local_addr: SocketAddr) -> SocketAddr {
    if !local_addr.ip().is_unspecified() {
        return local_addr;
    }
    StdUdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
        .and_then(|socket| {
            socket.connect(SocketAddr::from(([8, 8, 8, 8], 80)))?;
            socket.local_addr()
        })
        .map(|addr| SocketAddr::new(addr.ip(), local_addr.port()))
        .unwrap_or(local_addr)
}

pub(crate) fn announcement_targets(local_addr: SocketAddr) -> Vec<(SocketAddr, SocketAddr)> {
    if !local_addr.ip().is_unspecified() {
        return vec![(
            SocketAddr::new(local_addr.ip(), 0),
            advertised_addr(local_addr),
        )];
    }

    let mut targets: Vec<_> = system_network_interfaces()
        .into_iter()
        .filter(|(_, ip)| ip.is_ipv4())
        .map(|(_, ip)| {
            (
                SocketAddr::new(ip, 0),
                SocketAddr::new(ip, local_addr.port()),
            )
        })
        .collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        targets.push((
            SocketAddr::from(([0, 0, 0, 0], 0)),
            advertised_addr(local_addr),
        ));
    }
    targets
}

pub(crate) fn network_interfaces() -> Vec<NetworkInterfaceView> {
    let mut items: Vec<_> = system_network_interfaces()
        .into_iter()
        .map(network_interface_view)
        .collect();
    if items.is_empty() {
        items.push(network_interface_view((
            "本机测试".to_string(),
            IpAddr::from([127, 0, 0, 1]),
        )));
    }
    items
}

fn network_interface_view((name, ip): (String, IpAddr)) -> NetworkInterfaceView {
    NetworkInterfaceView {
        name,
        ip_addr: ip.to_string(),
        bind_addr: SocketAddr::new(ip, 0).to_string(),
        discovery_bind_addr: SocketAddr::new(ip, DISCOVERY_PORT).to_string(),
    }
}

fn system_network_interfaces() -> Vec<(String, IpAddr)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return fallback_network_interfaces();
    };
    let mut items: Vec<_> = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(addr) if !addr.ip.is_link_local() => {
                Some((interface.name, addr.ip.into()))
            }
            _ => None,
        })
        .collect();
    items.sort();
    items.dedup();
    items
}

fn fallback_network_interfaces() -> Vec<(String, IpAddr)> {
    let mut items = vec![("本机测试".to_string(), IpAddr::from([127, 0, 0, 1]))];
    if let Some(ip) = outbound_ip() {
        items.push(("当前出口网络".to_string(), ip));
    }
    items.sort();
    items.dedup();
    items
}

fn outbound_ip() -> Option<IpAddr> {
    StdUdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
        .and_then(|socket| {
            socket.connect(SocketAddr::from(([8, 8, 8, 8], 80)))?;
            socket.local_addr()
        })
        .map(|addr| addr.ip())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_addr_keeps_explicit_bind_address() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 9000));
        assert_eq!(advertised_addr(addr), addr);
    }

    #[test]
    fn announcement_targets_keep_explicit_bind_address() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 9000));
        assert_eq!(
            announcement_targets(addr),
            vec![(SocketAddr::from(([127, 0, 0, 1], 0)), addr)]
        );
    }
}
