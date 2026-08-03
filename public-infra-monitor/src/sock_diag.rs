use std::{
    io,
    mem::{self, size_of},
    net::{Ipv4Addr, Ipv6Addr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use anyhow::{Context, Result, bail};

use crate::model::{Endpoint, SocketObservation};

const NETLINK_SOCK_DIAG: libc::c_int = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLM_F_REQUEST: u16 = 0x01;
const NLM_F_ROOT: u16 = 0x100;
const NLM_F_MATCH: u16 = 0x200;
const NLMSG_ERROR: u16 = 0x02;
const NLMSG_DONE: u16 = 0x03;
const IPPROTO_TCP: u8 = 6;
const INET_DIAG_INFO: u16 = 2;
const INET_DIAG_SKMEMINFO: u16 = 7;
const SK_MEMINFO_DROPS: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NlMsgHeader {
    len: u32,
    kind: u16,
    flags: u16,
    seq: u32,
    pid: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InetDiagSockId {
    sport: u16,
    dport: u16,
    src: [u8; 16],
    dst: [u8; 16],
    interface: u32,
    cookie: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InetDiagRequest {
    family: u8,
    protocol: u8,
    extensions: u8,
    pad: u8,
    states: u32,
    id: InetDiagSockId,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InetDiagMessage {
    family: u8,
    state: u8,
    timer: u8,
    retrans: u8,
    id: InetDiagSockId,
    expires: u32,
    recv_queue: u32,
    send_queue: u32,
    uid: u32,
    inode: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RequestMessage {
    header: NlMsgHeader,
    request: InetDiagRequest,
}

pub fn query_tcp() -> Result<Vec<SocketObservation>> {
    let fd = open_netlink()?;
    let mut sockets = Vec::new();
    dump_family(&fd, libc::AF_INET as u8, 1, &mut sockets)?;
    dump_family(&fd, libc::AF_INET6 as u8, 2, &mut sockets)?;
    Ok(sockets)
}

fn open_netlink() -> Result<OwnedFd> {
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_SOCK_DIAG,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error()).context("open NETLINK_SOCK_DIAG");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let mut address: libc::sockaddr_nl = unsafe { mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as u16;
    let result = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error()).context("bind NETLINK_SOCK_DIAG");
    }
    Ok(fd)
}

fn dump_family(
    fd: &OwnedFd,
    family: u8,
    seq: u32,
    sockets: &mut Vec<SocketObservation>,
) -> Result<()> {
    let extensions = (1 << (INET_DIAG_INFO - 1)) | (1 << (INET_DIAG_SKMEMINFO - 1));
    let request = RequestMessage {
        header: NlMsgHeader {
            len: size_of::<RequestMessage>() as u32,
            kind: SOCK_DIAG_BY_FAMILY,
            flags: NLM_F_REQUEST | NLM_F_ROOT | NLM_F_MATCH,
            seq,
            pid: 0,
        },
        request: InetDiagRequest {
            family,
            protocol: IPPROTO_TCP,
            extensions: extensions as u8,
            pad: 0,
            states: u32::MAX,
            id: InetDiagSockId::default(),
        },
    };
    let sent = unsafe {
        libc::send(
            fd.as_raw_fd(),
            (&raw const request).cast(),
            size_of::<RequestMessage>(),
            0,
        )
    };
    if sent < 0 {
        return Err(io::Error::last_os_error()).context("send INET_DIAG dump request");
    }
    if sent as usize != size_of::<RequestMessage>() {
        bail!("short INET_DIAG request write: {sent}");
    }

    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let received = loop {
            let result =
                unsafe { libc::recv(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break result;
        };
        if received < 0 {
            return Err(io::Error::last_os_error()).context("receive INET_DIAG dump");
        }
        if received == 0 {
            bail!("unexpected EOF from NETLINK_SOCK_DIAG");
        }

        let mut offset = 0;
        let received = received as usize;
        while offset + size_of::<NlMsgHeader>() <= received {
            let header: NlMsgHeader = read_unaligned(&buffer[offset..])?;
            let message_len = header.len as usize;
            if message_len < size_of::<NlMsgHeader>() || offset + message_len > received {
                bail!("malformed INET_DIAG netlink message length {message_len}");
            }
            let payload = &buffer[offset + size_of::<NlMsgHeader>()..offset + message_len];
            if header.seq != seq {
                offset += align4(message_len);
                continue;
            }
            match header.kind {
                NLMSG_DONE => return Ok(()),
                NLMSG_ERROR => return parse_netlink_error(payload),
                SOCK_DIAG_BY_FAMILY => sockets.push(parse_diag_message(payload)?),
                _ => {}
            }
            offset += align4(message_len);
        }
    }
}

fn parse_netlink_error(payload: &[u8]) -> Result<()> {
    if payload.len() < size_of::<i32>() {
        bail!("short NETLINK error message");
    }
    let error = i32::from_ne_bytes(payload[..4].try_into().expect("checked length"));
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(-error)).context("INET_DIAG netlink error")
    }
}

fn parse_diag_message(payload: &[u8]) -> Result<SocketObservation> {
    let message: InetDiagMessage = read_unaligned(payload)?;
    let mut tcp_info = None;
    let mut socket_drops = None;
    let mut offset = align4(size_of::<InetDiagMessage>());
    while offset + 4 <= payload.len() {
        let length = u16::from_ne_bytes(payload[offset..offset + 2].try_into().unwrap()) as usize;
        let kind = u16::from_ne_bytes(payload[offset + 2..offset + 4].try_into().unwrap());
        if length < 4 || offset + length > payload.len() {
            break;
        }
        let value = &payload[offset + 4..offset + length];
        match kind {
            INET_DIAG_INFO => tcp_info = Some(TcpInfo::parse(value)),
            INET_DIAG_SKMEMINFO => socket_drops = read_u32(value, SK_MEMINFO_DROPS * 4),
            _ => {}
        }
        offset += align4(length);
    }
    let tcp_info = tcp_info.unwrap_or_default();

    Ok(SocketObservation {
        inode: message.inode as u64,
        family: family_name(message.family).to_owned(),
        state: tcp_state_name(message.state).to_owned(),
        local: Endpoint {
            address: format_address(message.family, &message.id.src),
            port: u16::from_be(message.id.sport),
        },
        remote: Endpoint {
            address: format_address(message.family, &message.id.dst),
            port: u16::from_be(message.id.dport),
        },
        recv_queue_bytes: message.recv_queue as u64,
        send_queue_bytes: message.send_queue as u64,
        rtt_us: tcp_info.rtt_us,
        rto_us: tcp_info.rto_us,
        snd_cwnd: tcp_info.snd_cwnd,
        last_data_recv_ms: tcp_info.last_data_recv_ms,
        bytes_received: tcp_info.bytes_received,
        bytes_sent: tcp_info.bytes_sent,
        total_retrans: tcp_info.total_retrans,
        socket_drops,
    })
}

#[derive(Default)]
struct TcpInfo {
    rto_us: Option<u32>,
    last_data_recv_ms: Option<u32>,
    rtt_us: Option<u32>,
    snd_cwnd: Option<u32>,
    total_retrans: Option<u32>,
    bytes_received: Option<u64>,
    bytes_sent: Option<u64>,
}

impl TcpInfo {
    fn parse(bytes: &[u8]) -> Self {
        Self {
            rto_us: read_u32(bytes, 8),
            last_data_recv_ms: read_u32(bytes, 52),
            rtt_us: read_u32(bytes, 68),
            snd_cwnd: read_u32(bytes, 80),
            total_retrans: read_u32(bytes, 100),
            bytes_received: read_u64(bytes, 128),
            bytes_sent: read_u64(bytes, 200),
        }
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn format_address(family: u8, bytes: &[u8; 16]) -> String {
    match family as libc::c_int {
        libc::AF_INET => Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        libc::AF_INET6 => Ipv6Addr::from(*bytes).to_string(),
        _ => "unknown".to_owned(),
    }
}

const fn family_name(family: u8) -> &'static str {
    match family as libc::c_int {
        libc::AF_INET => "ipv4",
        libc::AF_INET6 => "ipv6",
        _ => "unknown",
    }
}

const fn tcp_state_name(state: u8) -> &'static str {
    match state {
        1 => "ESTABLISHED",
        2 => "SYN_SENT",
        3 => "SYN_RECV",
        4 => "FIN_WAIT1",
        5 => "FIN_WAIT2",
        6 => "TIME_WAIT",
        7 => "CLOSE",
        8 => "CLOSE_WAIT",
        9 => "LAST_ACK",
        10 => "LISTEN",
        11 => "CLOSING",
        12 => "NEW_SYN_RECV",
        _ => "UNKNOWN",
    }
}

fn read_unaligned<T: Copy>(bytes: &[u8]) -> Result<T> {
    if bytes.len() < size_of::<T>() {
        bail!(
            "short binary structure: {} < {}",
            bytes.len(),
            size_of::<T>()
        );
    }
    Ok(unsafe { (bytes.as_ptr().cast::<T>()).read_unaligned() })
}

const fn align4(value: usize) -> usize {
    (value + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versioned_tcp_info_prefix() {
        let mut bytes = vec![0_u8; 208];
        bytes[8..12].copy_from_slice(&200_000_u32.to_ne_bytes());
        bytes[52..56].copy_from_slice(&17_u32.to_ne_bytes());
        bytes[68..72].copy_from_slice(&321_u32.to_ne_bytes());
        bytes[80..84].copy_from_slice(&10_u32.to_ne_bytes());
        bytes[100..104].copy_from_slice(&3_u32.to_ne_bytes());
        bytes[128..136].copy_from_slice(&123_456_u64.to_ne_bytes());
        bytes[200..208].copy_from_slice(&654_321_u64.to_ne_bytes());
        let info = TcpInfo::parse(&bytes);
        assert_eq!(info.rto_us, Some(200_000));
        assert_eq!(info.last_data_recv_ms, Some(17));
        assert_eq!(info.rtt_us, Some(321));
        assert_eq!(info.snd_cwnd, Some(10));
        assert_eq!(info.total_retrans, Some(3));
        assert_eq!(info.bytes_received, Some(123_456));
        assert_eq!(info.bytes_sent, Some(654_321));
    }

    #[test]
    fn tolerates_old_tcp_info_layout() {
        let info = TcpInfo::parse(&[0_u8; 104]);
        assert_eq!(info.total_retrans, Some(0));
        assert_eq!(info.bytes_received, None);
        assert_eq!(info.bytes_sent, None);
    }

    #[test]
    fn uapi_layouts_match_kernel_abi() {
        assert_eq!(size_of::<InetDiagSockId>(), 48);
        assert_eq!(size_of::<InetDiagRequest>(), 56);
        assert_eq!(size_of::<InetDiagMessage>(), 72);
    }
}
