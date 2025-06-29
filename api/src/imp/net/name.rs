use axerrno::LinuxResult;
use linux_raw_sys::net::{sockaddr, socklen_t};

use crate::{
    file::{FileLike, Socket},
    ptr::UserPtr,
    socket::SocketAddrExt,
};

pub fn sys_getsockname(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let local_addr = socket.local_addr()?;
    debug!("sys_getsockname <= fd: {}, addr: {:?}", fd, local_addr);

    *addrlen.get_as_mut()? = local_addr.write_to_user(addr)?;
    Ok(0)
}

pub fn sys_getpeername(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let peer_addr = socket.peer_addr()?;
    debug!("sys_getpeername <= fd: {}, addr: {:?}", fd, peer_addr);

    *addrlen.get_as_mut()? = peer_addr.write_to_user(addr)?;

    Ok(0)
}
