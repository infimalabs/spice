//! Addresses assigned to this host's network interfaces.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

/// Every IPv4 and IPv6 address assigned to a local interface, with port zero.
///
/// A link-local IPv6 address keeps its scope, without which a connection to
/// it names no interface. Interfaces that carry no address, or an address of
/// another family, contribute nothing.
///
/// # Errors
///
/// Returns the operating system's error when the interface table cannot be
/// read.
#[cfg(unix)]
pub fn local_addresses() -> io::Result<Vec<SocketAddr>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `head` is a valid destination for the list pointer; on success
    // the list is released below with `freeifaddrs`, exactly once.
    if unsafe { libc::getifaddrs(&raw mut head) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut addresses = Vec::new();
    let mut entry = head;
    while !entry.is_null() {
        // SAFETY: `entry` is a non-null node of the list `getifaddrs` returned,
        // which stays allocated until `freeifaddrs` below.
        let node = unsafe { &*entry };
        if let Some(address) = socket_address(node.ifa_addr) {
            addresses.push(address);
        }
        entry = node.ifa_next;
    }
    // SAFETY: `head` came from a successful `getifaddrs` and is freed once;
    // no reference into the list outlives this call.
    unsafe { libc::freeifaddrs(head) };
    Ok(addresses)
}

#[cfg(unix)]
fn socket_address(address: *const libc::sockaddr) -> Option<SocketAddr> {
    if address.is_null() {
        return None;
    }
    // SAFETY: a non-null interface address points at a sockaddr whose family
    // field is always readable.
    let family = libc::c_int::from(unsafe { (*address).sa_family });
    match family {
        libc::AF_INET => {
            // SAFETY: an AF_INET address is a complete `sockaddr_in`.
            let inet = unsafe { &*address.cast::<libc::sockaddr_in>() };
            Some(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(u32::from_be(inet.sin_addr.s_addr)),
                0,
            )))
        }
        libc::AF_INET6 => {
            // SAFETY: an AF_INET6 address is a complete `sockaddr_in6`.
            let inet6 = unsafe { &*address.cast::<libc::sockaddr_in6>() };
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(inet6.sin6_addr.s6_addr),
                0,
                0,
                inet6.sin6_scope_id,
            )))
        }
        _ => None,
    }
}

#[cfg(windows)]
pub fn local_addresses() -> io::Result<Vec<SocketAddr>> {
    use std::mem::size_of;
    use std::ptr;

    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_MULTICAST;
    let mut bytes = 0_u32;
    // SAFETY: a null buffer with a zero byte count is the documented sizing
    // call. No pointer is retained.
    let sizing = unsafe {
        GetAdaptersAddresses(
            u32::from(AF_UNSPEC),
            flags,
            ptr::null(),
            ptr::null_mut(),
            &raw mut bytes,
        )
    };
    if sizing != ERROR_BUFFER_OVERFLOW {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(sizing).unwrap_or(i32::MAX),
        ));
    }
    let words = usize::try_from(bytes)
        .expect("Windows adapter table length fits usize")
        .div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; words];
    let head = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: `storage` is aligned for pointers and spans at least `bytes`;
    // Windows initializes the linked adapter and unicast records within it.
    let status = unsafe {
        GetAdaptersAddresses(
            u32::from(AF_UNSPEC),
            flags,
            ptr::null(),
            head,
            &raw mut bytes,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ));
    }

    let mut addresses = Vec::new();
    let mut adapter = head;
    while !adapter.is_null() {
        // SAFETY: every adapter belongs to the initialized table in `storage`.
        let current = unsafe { &*adapter };
        let mut unicast = current.FirstUnicastAddress;
        while !unicast.is_null() {
            // SAFETY: every unicast record belongs to its adapter's table.
            let current = unsafe { &*unicast };
            if let Some(address) =
                windows_socket_address(current.Address.lpSockaddr, current.Address.iSockaddrLength)
            {
                addresses.push(address);
            }
            unicast = current.Next;
        }
        adapter = current.Next;
    }
    Ok(addresses)
}

#[cfg(windows)]
fn windows_socket_address(
    address: *const windows_sys::Win32::Networking::WinSock::SOCKADDR,
    length: i32,
) -> Option<SocketAddr> {
    use std::mem::size_of;

    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR_IN, SOCKADDR_IN6};

    if address.is_null() {
        return None;
    }
    // SAFETY: every address from GetAdaptersAddresses begins with SOCKADDR.
    let family = unsafe { (*address).sa_family };
    match family {
        AF_INET if usize::try_from(length).ok()? >= size_of::<SOCKADDR_IN>() => {
            // SAFETY: the family and recorded size establish SOCKADDR_IN.
            let inet = unsafe { &*address.cast::<SOCKADDR_IN>() };
            // SAFETY: S_addr is the active representation of an IPv4 address.
            let bytes = unsafe { inet.sin_addr.S_un.S_addr }.to_ne_bytes();
            Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(bytes), 0)))
        }
        AF_INET6 if usize::try_from(length).ok()? >= size_of::<SOCKADDR_IN6>() => {
            // SAFETY: the family and recorded size establish SOCKADDR_IN6.
            let inet = unsafe { &*address.cast::<SOCKADDR_IN6>() };
            // SAFETY: Byte is a complete byte representation of IN6_ADDR.
            let bytes = unsafe { inet.sin6_addr.u.Byte };
            // SAFETY: sin6_scope_id is the active scalar view of the scope.
            let scope = unsafe { inet.Anonymous.sin6_scope_id };
            Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(bytes),
                0,
                0,
                scope,
            )))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::local_addresses;

    #[test]
    fn the_loopback_interface_is_listed() {
        let addresses = local_addresses().expect("the interface table reads");
        assert!(
            addresses.iter().any(|address| address.ip().is_loopback()),
            "{addresses:?}"
        );
    }
}
