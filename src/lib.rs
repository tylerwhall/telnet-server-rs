#[macro_use]
extern crate futures;
extern crate libc;
#[macro_use]
extern crate log;
extern crate mio;
extern crate nix;
extern crate pty;
#[macro_use]
extern crate tokio_core;
extern crate tokio_io;

use std::io::{self, Read, Write};
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::net::ToSocketAddrs;
use std::process::{Child, Command, Stdio};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;

use futures::{Async, Future, Poll, Stream};
use mio::{Ready, Token, PollOpt};
use mio::unix::EventedFd;
use mio::event::Evented;
use tokio_core::reactor::{Core, Handle, PollEvented};
use tokio_core::net::{TcpListener, TcpStream};
use nix::fcntl::{self, FcntlArg, OFlag};

macro_rules! try_log {
    ($result:expr, $msg:expr) => {
        match $result {
            Ok(val) => val,
            Err(e) => {
                error!(concat!($msg, ": {:?}"), e);
                return Err(e.into());
            }
        }
    }
}

#[repr(u8)]
#[allow(non_camel_case_types, unused)]
enum TelnetCommand {
    /// interpret as command:
    IAC = 255,
    /// you are not to use option
    DONT = 254,
    /// please, you use option
    DO = 253,
    /// I won't use option
    WONT = 252,
    /// I will use option
    WILL = 251,
    /// interpret as subnegotiation
    SB = 250,
    /// you may reverse the line
    GA = 249,
    /// erase the current line
    EL = 248,
    /// erase the current character
    EC = 247,
    /// are you there
    AYT = 246,
    /// abort output--but let prog finish
    AO = 245,
    /// interrupt process--permanently
    IP = 244,
    /// break
    BREAK = 243,
    /// data mark--for connect. cleaning
    DM = 242,
    /// nop
    NOP = 241,
    /// end sub negotiation
    SE = 240,
    /// end of record (transparent mode)
    EOR = 239,
    /// Abort process
    ABORT = 238,
    /// Suspend process
    SUSP = 237,
    /// End of file: EOF is already used...
    xEOF = 236,
}

#[allow(non_camel_case_types, unused)]
enum TelnetOpt {
    /// 8-bit data path
    BINARY = 0,
    /// echo
    ECHO = 1,
    /// prepare to reconnect
    RCP = 2,
    /// suppress go ahead
    SGA = 3,
    /// approximate message size
    NAMS = 4,
    /// give status
    STATUS = 5,
    /// timing mark
    TM = 6,
    /// remote controlled transmission and echo
    RCTE = 7,
    /// negotiate about output line width
    NAOL = 8,
    /// negotiate about output page size
    NAOP = 9,
    /// negotiate about CR disposition
    NAOCRD = 10,
    /// negotiate about horizontal tabstops
    NAOHTS = 11,
    /// negotiate about horizontal tab disposition
    NAOHTD = 12,
    /// negotiate about formfeed disposition
    NAOFFD = 13,
    /// negotiate about vertical tab stops
    NAOVTS = 14,
    /// negotiate about vertical tab disposition
    NAOVTD = 15,
    /// negotiate about output LF disposition
    NAOLFD = 16,
    /// extended ascii character set
    XASCII = 17,
    /// force logout
    LOGOUT = 18,
    /// byte macro
    BM = 19,
    /// data entry terminal
    DET = 20,
    /// supdup protocol
    SUPDUP = 21,
    /// supdup output
    SUPDUPOUTPUT = 22,
    /// send location
    SNDLOC = 23,
    /// terminal type
    TTYPE = 24,
    /// end or record
    EOR = 25,
    /// TACACS user identification
    TUID = 26,
    /// output marking
    OUTMRK = 27,
    /// terminal location number
    TTYLOC = 28,
    /// 3270 regime
    REGIME3270 = 29,
    /// X.3 PAD
    X3PAD = 30,
    /// window size
    NAWS = 31,
    /// terminal speed
    TSPEED = 32,
    /// remote flow control
    LFLOW = 33,
    /// Linemode option
    LINEMODE = 34,
    /// X Display Location
    XDISPLOC = 35,
    /// Old - Environment variables
    OLD_ENVIRON = 36,
    /// Authenticate
    AUTHENTICATION = 37,
    /// Encryption option
    ENCRYPT = 38,
    /// New - Environment variables
    NEW_ENVIRON = 39,
    /// extended-options-list
    EXOPL = 255,
}

#[derive(Debug)]
struct PtyMaster {
    master: File,
    masterfd: i32,
}

impl PtyMaster {
    fn new() -> io::Result<(Self, File)> {
        let (master, ptsname) = pty::fork::Master::new(b"/dev/ptmx\0".as_ptr() as *const libc::c_char)
            .and_then(|master| master.grantpt().map(|_| master))
            .and_then(|master| master.unlockpt().map(|_| master))
            .and_then(|master| master.ptsname().map(|name| (master, name)))
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let ptsname = unsafe { CStr::from_ptr(ptsname).to_str().expect("ptsname to str") };
        let slave = try_log!(OpenOptions::new().read(true).write(true).open(ptsname), "open pts failed");

        let masterfd = master.as_raw_fd();

        Ok((PtyMaster {
            master: unsafe { File::from_raw_fd(masterfd) },
            masterfd: masterfd,
        }, slave))
    }

    fn evented(self) -> Result<PtyMasterEvented, nix::Error> {
        fcntl::fcntl(self.masterfd, FcntlArg::F_GETFL)
            .map(|fl| OFlag::from_bits_truncate(fl))
            .and_then(|fl| {
                fcntl::fcntl(self.masterfd, FcntlArg::F_SETFL(fl | fcntl::O_NONBLOCK))
            }).map(|_| PtyMasterEvented(self))
    }
}

struct PtyMasterEvented(PtyMaster);

impl Evented for PtyMasterEvented {
    fn register(&self, poll: &mio::Poll, token: Token, interest: Ready, opts: PollOpt) -> io::Result<()> {
        let evented = EventedFd(&self.0.masterfd);
        evented.register(poll, token, interest, opts)
    }

    fn reregister(&self, poll: &mio::Poll, token: Token, interest: Ready, opts: PollOpt) -> io::Result<()> {
        let evented = EventedFd(&self.0.masterfd);
        evented.reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
        let evented = EventedFd(&self.0.masterfd);
        evented.deregister(poll)
    }
}

impl Read for PtyMasterEvented {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.master.read(buf)
    }
}

impl Write for PtyMasterEvented {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.master.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.master.flush()
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
enum TelnetRequest {
    Do = 254,
    Dont = 253,
    Wont = 252,
    Will = 251,
}

impl TelnetRequest {
    fn from_u8(val: u8) -> Option<Self> {
        if val == TelnetRequest::Do as u8 {
            Some(TelnetRequest::Do)
        } else if val == TelnetRequest::Dont as u8 {
            Some(TelnetRequest::Dont)
        } else if val == TelnetRequest::Will as u8 {
            Some(TelnetRequest::Will)
        } else if val == TelnetRequest::Wont as u8 {
            Some(TelnetRequest::Wont)
        } else {
            None
        }
    }
}

struct AsyncBufferedReader {
    read_buf: [u8; 128],
    read_pos: usize,
    read_len: usize,
}

impl AsyncBufferedReader {
    fn new() -> Self {
        AsyncBufferedReader {
            read_buf: [0; 128],
            read_pos: 0,
            read_len: 0,
        }
    }

    // Return slice of bytes ready (at least 1), Error on EOF or other error, or NotReady
    fn borrow_bytes<R: Read>(&mut self, mut reader: R) -> Poll<&[u8], io::Error> {
        // If any bytes ready, return those.
        if self.read_len > self.read_pos {
            return Ok(Async::Ready(&self.read_buf[self.read_pos..self.read_len]));
        }

        self.read_len = try_nb!(reader.read(&mut self.read_buf));
        if self.read_len == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into())
        }
        self.read_pos = 0;
        Ok(Async::Ready(&self.read_buf[self.read_pos..self.read_len]))
    }

    // Return the next byte without consuming it
    fn peek_byte<R: Read>(&mut self, reader: R) -> Poll<u8, io::Error> {
        Ok(try_ready!(self.borrow_bytes(reader))[0].into())
    }

    // Return the next byte, consuming it
    fn get_byte<R: Read>(&mut self, reader: R) -> Poll<u8, io::Error> {
        let ret = try_ready!(self.peek_byte(reader));
        self.consume(1);
        Ok(ret.into())
    }

    // Discard bytes already read. Meant to be used with borrow_bytes
    fn consume(&mut self, bytes: usize) {
        self.read_pos += bytes;
        assert!(self.read_len >= self.read_pos);
    }
}

struct TelnetReply {
    escape_needed: bool,
    rsp: Option<TelnetRequest>,
    val: u8,
}

impl TelnetReply {
    fn new(rsp: TelnetRequest, val: u8) -> Self {
        TelnetReply {
            escape_needed: true,
            rsp: Some(rsp),
            val: val,
        }
    }

    fn send<W: Write>(&mut self, mut w: W) -> Poll<(), io::Error> {
        if self.escape_needed {
            try_nb!(w.write(&[TelnetCommand::IAC as u8]));
            self.escape_needed = false;
        }
        if let Some(rsp) = self.rsp {
            try_nb!(w.write(&[rsp as u8]));
            self.rsp = None;
        }
        try_nb!(w.write(&[self.val]));
        Ok(().into())
    }
}

enum TelnetInputState {
    Normal,
    Escape,
    /// Got request. Need argument.
    Request(TelnetRequest),
}

enum TelnetOutputState {
    Normal,
    Escape,
}

struct TelnetConnection<P: Read + Write, S: Read + Write> {
    pty: P,
    stream: S,

    stream_reader: AsyncBufferedReader,
    input_state: TelnetInputState,
    pending_reply: Option<TelnetReply>,

    pty_reader: AsyncBufferedReader,
    output_state: TelnetOutputState,
}

impl<P: Read + Write, S: Read + Write> TelnetConnection<P, S> {
    fn new(m: P, s: S) -> Self {
        TelnetConnection {
            pty: m,
            stream: s,

            stream_reader: AsyncBufferedReader::new(),
            input_state: TelnetInputState::Normal,
            pending_reply: None,

            pty_reader: AsyncBufferedReader::new(),
            output_state: TelnetOutputState::Normal,
        }
    }

    fn handle_stream_rx(&mut self) -> Poll<(), io::Error> {
        loop {
            match self.input_state {
                TelnetInputState::Normal => {
                    let consumed = {
                        let write_buf = try_ready!(self.stream_reader.borrow_bytes(&mut self.stream));
                        // Split the buffer at the first escape
                        let mut iter = write_buf.split(|val| *val == TelnetCommand::IAC as u8);
                        let data = iter.next().unwrap();
                        // Write any data before the escape
                        let consumed = if data.len() > 0 {
                            try_nb!(self.pty.write(data))
                        } else {
                            0
                        };
                        consumed + if let Some(_) = iter.next() {
                            trace!("Got escape");
                            self.input_state = TelnetInputState::Escape;
                            1
                        } else {
                            0
                        }
                    };
                    self.stream_reader.consume(consumed);
                }
                TelnetInputState::Escape => {
                    let cmd = try_ready!(self.stream_reader.peek_byte(&mut self.stream));
                    trace!("Escape val {}", cmd);
                    if let Some(req) = TelnetRequest::from_u8(cmd) {
                        // If a request, read the argument
                        self.input_state = TelnetInputState::Request(req)
                    } else if cmd == TelnetCommand::IAC as u8 {
                        // If an escaped 0xff, pass it through
                        try_nb!(self.pty.write(&[cmd]));
                        self.input_state = TelnetInputState::Normal
                    } else {
                        // TODO: handle escape commands
                        self.input_state = TelnetInputState::Normal
                    }
                    self.stream_reader.consume(1)
                }
                TelnetInputState::Request(req) => {
                    if self.pending_reply.is_some() {
                        // Still need to send the last reply, so we can't queue another
                        return Ok(Async::NotReady);
                    }
                    let reqval = try_ready!(self.stream_reader.get_byte(&mut self.stream));
                    trace!("Req {:?} {}", req, reqval);
                    self.pending_reply = Some(TelnetReply::new(TelnetRequest::Wont, reqval));
                    self.input_state = TelnetInputState::Normal;
                }
            }
        }
    }

    fn handle_stream_tx(&mut self) -> Poll<(), io::Error> {
        // Send replies before normal data
        if let Some(ref mut reply) = self.pending_reply {
            try_ready!(reply.send(&mut self.stream));
        }
        self.pending_reply = None;

        loop {
            match self.output_state {
                TelnetOutputState::Normal => {
                    let (consumed, escape) = {
                        trace!("Reading from pty");
                        let write_buf = try_ready!(self.pty_reader.borrow_bytes(&mut self.pty));
                        trace!("Got {} from pty", write_buf.len());
                        // Split the buffer at the first escape
                        let mut iter = write_buf.split(|val| *val == TelnetCommand::IAC as u8);
                        let data = iter.next().unwrap();
                        // Write any data before the escape
                        let consumed = if data.len() > 0 {
                            trace!("Write {} from pty to stream", data.len());
                            try_nb!(self.stream.write(data))
                        } else {
                            0
                        };
                        trace!("Wrote {} from pty to stream", data.len());
                        let escape = if let Some(_) = iter.next() {
                            true
                        } else {
                            false
                        };
                        (consumed, escape)
                    };
                    self.pty_reader.consume(consumed);
                    if escape {
                        try_nb!(self.stream.write(&[TelnetCommand::IAC as u8]));
                        self.pty_reader.consume(1);
                        self.output_state = TelnetOutputState::Escape;
                    }
                }
                TelnetOutputState::Escape => {
                    try_nb!(self.stream.write(&[TelnetCommand::IAC as u8]));
                    self.output_state = TelnetOutputState::Normal;
                }
            }
        }
    }
}


impl<P: Read + Write, S: Read + Write> Future for TelnetConnection<P, S> {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        trace!("Handle rx");
        self.handle_stream_rx()?;
        trace!("Handle tx");
        self.handle_stream_tx()
    }
}

struct TelnetShell {
    tc: TelnetConnection<PollEvented<PtyMasterEvented>, TcpStream>,
    child: Child,
}

impl TelnetShell {
    fn new(stream: TcpStream, handle: &Handle) -> io::Result<Self> {
        let (master, slave) = PtyMaster::new()?;
        let master = try_log!(master.evented(), "creating evented master");
        let master = PollEvented::new(master, &handle).expect("poll evented");

        let slavefd = slave.as_raw_fd();
        let stdin = unsafe { Stdio::from_raw_fd(slavefd) };
        let stdout = unsafe { Stdio::from_raw_fd(slavefd) };
        let stderr = unsafe { Stdio::from_raw_fd(slavefd) };

        let child = try_log!(Command::new("/bin/sh")
            .arg("-l")
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .before_exec(|| {
                let _ = nix::unistd::setsid();
                unsafe { libc::ioctl(0, libc::TIOCSCTTY) };
                Ok(())
            })
            .spawn(), "shell spawn failed");

        Ok(TelnetShell {
            tc: TelnetConnection::new(master, stream),
            child: child,
        })
    }
}

impl Future for TelnetShell {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.tc.poll()
    }
}

impl Drop for TelnetShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn telnet_serve<A: ToSocketAddrs>(addr: A) {
    let mut core = Core::new().unwrap();
    let handle = core.handle();

    let addr = addr.to_socket_addrs().unwrap().nth(0).unwrap();
    let listener = TcpListener::bind(&addr, &handle).unwrap();

    let server = listener.incoming().for_each(|(stream, _)| {
        let addr = stream.peer_addr()?;
        info!("Telnet connection from {:?}", addr);

        match TelnetShell::new(stream, &handle) {
            Err(e) => error!("Connection error {:?}", e),
            Ok(shell) => {
                handle.spawn(shell.then(move |_| {
                    info!("Telnet connection from {:?} closed", addr);
                    Ok(())
                }));
            },
        }
        Ok(())
    });

    core.run(server).expect("Telnet server died");
}
