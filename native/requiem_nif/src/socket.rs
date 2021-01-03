use rustler::env::OwnedEnv;
use rustler::types::binary::{Binary, OwnedBinary};
use rustler::types::tuple::make_tuple;
use rustler::types::{Encoder, LocalPid};
use rustler::{Atom, Env, NifResult, ResourceArc};

use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};

use std::collections::HashMap;
use std::convert::TryInto;
use std::net::{IpAddr, SocketAddr};
use std::str;
use std::thread;
use std::time;

use crate::common::{self, atoms};

type ModuleName = Vec<u8>;
// type SocketCloser = RwLock<HashMap<ModuleName, RwLock<bool>>>;
type SenderSocket = RwLock<HashMap<ModuleName, Mutex<std::net::UdpSocket>>>;

// static CLOSERS: Lazy<SocketCloser> = Lazy::new(|| RwLock::new(HashMap::new()));
static SOCKETS: Lazy<SenderSocket> = Lazy::new(|| RwLock::new(HashMap::new()));

pub struct Peer {
    addr: SocketAddr,
}

impl Peer {
    pub fn new(addr: SocketAddr) -> Self {
        Peer { addr: addr }
    }
}

pub struct Socket {
    sock: UdpSocket,
    poll: Poll,
    events: Events,
    buf: [u8; 65535],
}

impl Socket {
    pub fn new(sock: std::net::UdpSocket, event_capacity: usize) -> Self {
        let buf = [0; 65535];
        let mut sock = UdpSocket::from_std(sock);

        let poll = Poll::new().unwrap();

        poll.registry()
            .register(&mut sock, Token(0), Interest::READABLE)
            .unwrap();

        let events = Events::with_capacity(event_capacity);

        Socket {
            sock: sock,
            poll: poll,
            events: events,
            buf: buf,
        }
    }

    pub fn poll(&mut self, env: &Env, pid: &LocalPid, interval: u64) {
        let timeout = time::Duration::from_millis(interval);
        self.poll.poll(&mut self.events, Some(timeout)).unwrap();

        for event in self.events.iter() {
            match event.token() {
                Token(0) => {
                    let (len, peer) = match self.sock.recv_from(&mut self.buf) {
                        Ok(v) => v,
                        Err(e) => {
                            if e.kind() != std::io::ErrorKind::WouldBlock {
                                env.send(
                                    pid,
                                    make_tuple(
                                        *env,
                                        &[
                                            atoms::socket_error().to_term(*env),
                                            atoms::cant_receive().to_term(*env),
                                        ],
                                    ),
                                );
                            }
                            return;
                        }
                    };

                    if len < 4 {
                        // too short packet. ignore
                        return;
                    }

                    if len > 1350 {
                        // too big packet. ignore
                        return;
                    }

                    let mut packet = OwnedBinary::new(len).unwrap();
                    packet.as_mut_slice().copy_from_slice(&self.buf[..len]);

                    env.send(
                        pid,
                        make_tuple(
                            *env,
                            &[
                                atoms::__packet__().to_term(*env),
                                ResourceArc::new(Peer::new(peer)).encode(*env),
                                packet.release(*env).to_term(*env),
                            ],
                        ),
                    );
                }
                _ => {}
            }
        }
    }
}

#[rustler::nif]
pub fn socket_open(
    module: Binary,
    address: Binary,
    pid: LocalPid,
    event_capacity: u64,
    poll_interval: u64,
) -> NifResult<Atom> {
    let module = module.as_slice();

    let address = str::from_utf8(address.as_slice()).unwrap();

    let std_sock = std::net::UdpSocket::bind(address).unwrap();
    let std_sock2 = std_sock.try_clone().unwrap();

    let cap = event_capacity.try_into().unwrap();
    let mut receiver = Socket::new(std_sock2, cap);
    let oenv = OwnedEnv::new();
    thread::spawn(move || {
        oenv.run(move |env| loop {
            receiver.poll(&env, &pid, poll_interval);
        })
    });

    let mut socket_table = SOCKETS.write();
    if !socket_table.contains_key(module) {
        socket_table.insert(module.to_vec(), Mutex::new(std_sock));
    }

    Ok(atoms::ok())
}

#[rustler::nif]
pub fn socket_send(module: Binary, peer: ResourceArc<Peer>, packet: Binary) -> NifResult<Atom> {
    let module = module.as_slice();
    let socket_table = SOCKETS.read();
    if let Some(socket) = socket_table.get(module) {
        let socket = socket.lock();
        match socket.send_to(packet.as_slice(), &peer.addr) {
            Ok(_size) => Ok(atoms::ok()),
            Err(_) => Err(common::error_term(atoms::system_error())),
        }
    } else {
        Err(common::error_term(atoms::not_found()))
    }
}

#[rustler::nif]
pub fn socket_close(module: Binary) -> NifResult<Atom> {
    let module = module.as_slice();

    let mut socket_table = SOCKETS.write();
    if socket_table.contains_key(module) {
        socket_table.remove(module);
    }
    Ok(atoms::ok())
}

#[rustler::nif]
pub fn socket_address_parts(env: Env, peer: ResourceArc<Peer>) -> NifResult<(Atom, Binary, u16)> {
    let ip_bytes = match peer.addr.ip() {
        IpAddr::V4(ip) => ip.octets().to_vec(),
        IpAddr::V6(ip) => ip.octets().to_vec(),
    };

    let mut ip = OwnedBinary::new(ip_bytes.len()).unwrap();
    ip.as_mut_slice().copy_from_slice(&ip_bytes);

    Ok((atoms::ok(), ip.release(env), peer.addr.port()))
}

pub fn on_load(env: Env) -> bool {
    rustler::resource!(Peer, env);
    true
}
