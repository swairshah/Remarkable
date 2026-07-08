//! The tool socket: how pi's notebook_draw / notebook_erase / notebook_view
//! tools reach back into the running app.
//!
//! pi's extension executes inside pi's node process, so it can't touch the
//! page model directly — instead the app listens on a unix socket
//! ($NOTEBOOK_SOCK) speaking one JSON object per line, request/response.
//! This module is transport only: accept, buffer, split lines, parse; the
//! command semantics live in main.rs, which owns the page and the panel.
//!
//! Everything is nonblocking and polled from the main loop like every other
//! fd in the app; a stuck client can't stall the pen.

use serde_json::Value;
use std::io;
use std::os::unix::io::RawFd;

pub struct Conn {
    pub fd: RawFd,
    buf: Vec<u8>,
    dead: bool,
}

pub struct IpcServer {
    listen_fd: RawFd,
    pub path: String,
    pub conns: Vec<Conn>,
}

fn set_nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL, 0);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

impl IpcServer {
    pub fn open(path: &str) -> io::Result<IpcServer> {
        let _ = std::fs::remove_file(path); /* stale socket from a crash */
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        if path.len() >= addr.sun_path.len() {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path too long"));
        }
        for (i, b) in path.bytes().enumerate() {
            addr.sun_path[i] = b as libc::c_char;
        }
        let rc = unsafe {
            libc::bind(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if rc != 0 || unsafe { libc::listen(fd, 8) } != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        set_nonblock(fd);
        println!("notebook: tool socket at {path}");
        Ok(IpcServer { listen_fd: fd, path: path.to_string(), conns: Vec::new() })
    }

    pub fn listen_fd(&self) -> RawFd {
        self.listen_fd
    }

    /// Accept any pending connections (call when the listen fd polls in).
    pub fn accept(&mut self) {
        loop {
            let fd = unsafe { libc::accept(self.listen_fd, std::ptr::null_mut(), std::ptr::null_mut()) };
            if fd < 0 {
                return; /* EAGAIN: done */
            }
            set_nonblock(fd);
            self.conns.push(Conn { fd, buf: Vec::new(), dead: false });
        }
    }

    /// Read whatever a connection has and return complete-line requests.
    pub fn read_conn(&mut self, fd: RawFd) -> Vec<Value> {
        let mut out = Vec::new();
        let Some(conn) = self.conns.iter_mut().find(|c| c.fd == fd) else {
            return out;
        };
        loop {
            let mut chunk = [0u8; 8192];
            let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
            if n > 0 {
                conn.buf.extend_from_slice(&chunk[..n as usize]);
                if conn.buf.len() > 4 << 20 {
                    conn.dead = true; /* runaway client */
                    break;
                }
                continue;
            }
            if n == 0 {
                conn.dead = true;
            }
            break; /* EAGAIN or EOF */
        }
        while let Some(pos) = conn.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = conn.buf.drain(..=pos).collect();
            let line = &line[..line.len() - 1];
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Value>(line) {
                Ok(v) => out.push(v),
                Err(e) => eprintln!("notebook: bad ipc json: {e}"),
            }
        }
        self.reap();
        out
    }

    /// Send one JSON response line on a connection. Best-effort: the write
    /// is blocking-ish but responses are small (view PNGs are the largest,
    /// a few hundred KB, well within socket buffers + a short spin).
    pub fn reply(&mut self, fd: RawFd, v: &Value) {
        let mut line = serde_json::to_vec(v).unwrap_or_default();
        line.push(b'\n');
        let mut off = 0;
        let mut spins = 0;
        while off < line.len() {
            let n = unsafe {
                libc::write(fd, line[off..].as_ptr() as *const libc::c_void, line.len() - off)
            };
            if n > 0 {
                off += n as usize;
                spins = 0;
                continue;
            }
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::WouldBlock && spins < 400 {
                spins += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if let Some(c) = self.conns.iter_mut().find(|c| c.fd == fd) {
                c.dead = true;
            }
            break;
        }
        self.reap();
    }

    fn reap(&mut self) {
        self.conns.retain(|c| {
            if c.dead {
                unsafe { libc::close(c.fd) };
            }
            !c.dead
        });
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        for c in &self.conns {
            unsafe { libc::close(c.fd) };
        }
        unsafe { libc::close(self.listen_fd) };
        let _ = std::fs::remove_file(&self.path);
    }
}
