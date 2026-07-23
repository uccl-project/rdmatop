/// Netlink message builder, parser, and socket for RDMA netlink.
use crate::rdma::*;
use std::io;
use std::mem;
use std::os::unix::io::RawFd;

const NLMSG_HDR_LEN: usize = 16;
const NLA_HDR_LEN: usize = 4;
const NLA_ALIGN: usize = 4;

fn align(len: usize) -> usize {
    (len + NLA_ALIGN - 1) & !(NLA_ALIGN - 1)
}

/// Parsed netlink attribute.
pub struct Nla<'a> {
    pub attr_type: u16,
    pub data: &'a [u8],
}

impl<'a> Nla<'a> {
    pub fn u8(&self) -> u8 {
        self.data.first().copied().unwrap_or(0)
    }

    pub fn u32(&self) -> u32 {
        if self.data.len() < 4 {
            return 0;
        }
        u32::from_ne_bytes(self.data[..4].try_into().unwrap())
    }

    pub fn u64(&self) -> u64 {
        if self.data.len() < 8 {
            return 0;
        }
        u64::from_ne_bytes(self.data[..8].try_into().unwrap())
    }

    pub fn str(&self) -> &str {
        let end = self
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.data.len());
        std::str::from_utf8(&self.data[..end]).unwrap_or("")
    }

    /// Iterate child attributes within a nested attribute.
    pub fn nested(&self) -> NlaIter<'a> {
        NlaIter(self.data)
    }
}

/// Iterator over netlink attributes in a byte buffer.
pub struct NlaIter<'a>(pub(crate) &'a [u8]);

impl<'a> Iterator for NlaIter<'a> {
    type Item = Nla<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.len() < NLA_HDR_LEN {
            return None;
        }
        let len = u16::from_ne_bytes([self.0[0], self.0[1]]) as usize;
        let atype = u16::from_ne_bytes([self.0[2], self.0[3]]);
        if len < NLA_HDR_LEN || len > self.0.len() {
            return None;
        }
        let nla = Nla {
            attr_type: atype & !NLA_F_NESTED,
            data: &self.0[NLA_HDR_LEN..len],
        };
        self.0 = &self.0[align(len).min(self.0.len())..];
        Some(nla)
    }
}

/// Constructs a netlink message with chained attribute appends.
pub struct NlMsgBuilder {
    buf: Vec<u8>,
}

impl NlMsgBuilder {
    pub fn new(msg_type: u16, flags: u16, seq: u32) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&(NLMSG_HDR_LEN as u32).to_ne_bytes());
        buf.extend_from_slice(&msg_type.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        Self { buf }
    }

    pub fn put_u32(mut self, attr_type: u16, val: u32) -> Self {
        let len: u16 = (NLA_HDR_LEN + 4) as u16;
        self.buf.extend_from_slice(&len.to_ne_bytes());
        self.buf.extend_from_slice(&attr_type.to_ne_bytes());
        self.buf.extend_from_slice(&val.to_ne_bytes());
        self
    }

    /// Finalize the message, patching the total length in the header.
    pub fn build(mut self) -> Vec<u8> {
        let len = self.buf.len() as u32;
        self.buf[0..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }

    /// Append raw bytes (e.g. for ifinfomsg struct payload).
    pub fn put_raw(mut self, data: &[u8]) -> Self {
        self.buf.extend_from_slice(data);
        self
    }
}

/// Iterator over netlink messages in a recv buffer.
pub struct NlMsgIter<'a> {
    buf: &'a [u8],
}

impl<'a> NlMsgIter<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
}

/// A single netlink message.
pub struct NlMsg<'a> {
    pub msg_type: u16,
    pub payload: &'a [u8],
}

impl<'a> NlMsg<'a> {
    pub fn attrs(&self) -> NlaIter<'a> {
        NlaIter(self.payload)
    }
    pub fn is_done(&self) -> bool {
        self.msg_type == NLMSG_DONE
    }
    pub fn is_error(&self) -> bool {
        self.msg_type == NLMSG_ERROR
    }
}

impl<'a> Iterator for NlMsgIter<'a> {
    type Item = NlMsg<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.len() < NLMSG_HDR_LEN {
            return None;
        }
        let len = u32::from_ne_bytes(self.buf[..4].try_into().unwrap()) as usize;
        let msg_type = u16::from_ne_bytes(self.buf[4..6].try_into().unwrap());
        if len < NLMSG_HDR_LEN || len > self.buf.len() {
            return None;
        }
        let msg = NlMsg {
            msg_type,
            payload: &self.buf[NLMSG_HDR_LEN..len],
        };
        self.buf = &self.buf[align(len).min(self.buf.len())..];
        Some(msg)
    }
}

/// RDMA netlink socket.
pub struct NlSocket {
    fd: RawFd,
}

impl NlSocket {
    /// Open a netlink socket for the given protocol and bind it.
    pub fn open(proto: i32) -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, proto) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        let ret = unsafe {
            libc::bind(
                fd,
                &addr as *const _ as *const _,
                mem::size_of_val(&addr) as u32,
            )
        };
        if ret < 0 {
            unsafe { libc::close(fd) };
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<()> {
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        let ret = unsafe {
            libc::sendto(
                self.fd,
                buf.as_ptr() as *const _,
                buf.len(),
                0,
                &addr as *const _ as *const _,
                mem::size_of_val(&addr) as u32,
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn recv(&self) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; 65536];
        let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// Send a request and collect all responses until DONE or ERROR.
    pub fn request(&self, msg: Vec<u8>) -> io::Result<Vec<Vec<u8>>> {
        self.send(&msg)?;
        let mut bufs = Vec::new();
        loop {
            let resp = self.recv()?;
            let done = NlMsgIter::new(&resp).any(|m| m.is_done() || m.is_error());
            bufs.push(resp);
            if done {
                return Ok(bufs);
            }
        }
    }
}

/// Send a request and collect all parsed responses until DONE or ERROR.
pub fn collect_responses<T>(
    sock: &NlSocket,
    msg: Vec<u8>,
    parse: fn(&NlMsg) -> Option<T>,
) -> io::Result<Vec<T>> {
    let mut results = Vec::new();
    for buf in sock.request(msg)? {
        for nlmsg in NlMsgIter::new(&buf) {
            if nlmsg.is_done() || nlmsg.is_error() {
                continue;
            }
            if let Some(item) = parse(&nlmsg) {
                results.push(item);
            }
        }
    }
    Ok(results)
}

impl Drop for NlSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
