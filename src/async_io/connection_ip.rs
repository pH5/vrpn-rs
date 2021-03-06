// Copyright 2018, Collabora, Ltd.
// SPDX-License-Identifier: BSL-1.0
// Author: Ryan A. Pavlik <ryan.pavlik@collabora.com>

use crate::{
    async_io::{connect::incoming_handshake, endpoint_ip::EndpointIp},
    connection::*,
    Error, LogFileNames, Result, TypeSafeId,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    net::{tcp::Incoming, TcpListener, TcpStream},
    prelude::*,
};

#[derive(Debug)]
pub struct ConnectionIp {
    core: ConnectionCore<EndpointIp>,
    // server_tcp: Option<Mutex<TcpListener>>,
    server_acceptor: Arc<Mutex<Option<ConnectionIpAcceptor>>>,
}
const DEFAULT_PORT: u16 = 3883;

impl ConnectionIp {
    /// Create a new ConnectionIp that is a server.
    pub fn new_server(
        local_log_names: Option<LogFileNames>,
        _addr: Option<SocketAddr>,
    ) -> Result<Arc<ConnectionIp>> {
        let conn = Arc::new(ConnectionIp {
            core: ConnectionCore::new(Vec::new(), local_log_names, None),
            server_acceptor: Arc::new(Mutex::new(None)),
            // server_tcp: Some(Mutex::new(server_tcp)),
        });
        // {
        //     let accepter = ConnectionIpAcceptor::new(Arc::downgrade(&conn), addr)?;
        //     let mut locked_acceptor = conn.server_acceptor.lock()?;
        //     *locked_acceptor = Some(accepter);
        // }
        Ok(conn)
    }

    /// Create a new ConnectionIp that is a client.
    pub fn new_client(
        local_log_names: Option<LogFileNames>,
        remote_log_names: Option<LogFileNames>,
        reliable_channel: TcpStream,
        // low_latency_channel: Option<MessageFramedUdp>,
    ) -> Result<Arc<ConnectionIp>> {
        let mut endpoints: Vec<Option<EndpointIp>> = Vec::new();
        endpoints.push(Some(EndpointIp::new(reliable_channel)));
        Ok(Arc::new(ConnectionIp {
            core: ConnectionCore::new(endpoints, local_log_names, remote_log_names),
            server_acceptor: Arc::new(Mutex::new(None)),
        }))
    }

    pub fn poll_endpoints(&self) -> Poll<Option<()>, Error> {
        // eprintln!("in <ConnectionIp as Future>::poll");
        // if let Some(listener_mutex) = &self.server_tcp {
        //     let listener = listener_mutex.lock()?;
        //     match listener.incoming().poll()? {
        //         Async::Ready(Some(sock)) => {
        //             // OK, we got a new one.
        //             let endpoints = self.endpoints();
        //             tokio::spawn(
        //                 incoming_handshake(sock)
        //                     .and_then(move |stream| {
        //                         if let Ok(mut epoints) = endpoints.lock() {
        //                             epoints.push(Some(EndpointIp::new(stream)));
        //                         }
        //                         Ok(())
        //                     })
        //                     .map_err(|e| {
        //                         eprintln!("err: {:?}", e);
        //                     }),
        //             );
        //         }
        //         Async::Ready(None) => return Ok(Async::Ready(None)),
        //         Async::NotReady => (),
        //     }
        // }
        let mut acceptor = self.server_acceptor.lock()?;
        match &mut (*acceptor) {
            Some(a) => loop {
                let poll_result = a.poll()?;
                match poll_result {
                    Async::NotReady => break,
                    Async::Ready(Some(_)) => (),
                    Async::Ready(None) => return Ok(Async::Ready(None)),
                }
            },
            None => (),
        }
        let endpoints = self.endpoints();
        let dispatcher = self.dispatcher();
        {
            let mut endpoints = endpoints.lock()?;
            let mut dispatcher = dispatcher.lock()?;
            // eprintln!("dispatcher:");
            // for (id, name) in dispatcher.senders_iter() {
            //     eprintln!("  sender {}: {:?}", id.get(), name.0);
            // }
            // for (id, name) in dispatcher.types_iter() {
            //     eprintln!("  type {}: {:?}", id.get(), name.0);
            // }
            let mut got_not_ready = false;
            for ep in endpoints.iter_mut().flatten() {
                let poll_result = ep.poll_endpoint(&mut dispatcher)?;
                match poll_result {
                    Async::Ready(()) => {
                        eprintln!("endpoint closed apparently");
                        // TODO do we delete this?
                        //return Ok(Async::Read);
                    }
                    Async::NotReady => {
                        got_not_ready = true;
                        // this is normal.
                    }
                }
            }
            if got_not_ready {
                Ok(Async::NotReady)
            } else {
                Ok(Async::Ready(Some(())))
            }
        }
    }
}

impl Connection for ConnectionIp {
    type SpecificEndpoint = EndpointIp;
    fn connection_core(&self) -> &ConnectionCore<Self::SpecificEndpoint> {
        &self.core
    }
}

#[derive(Debug)]
pub struct ConnectionIpStream {
    connection: Arc<ConnectionIp>,
}

impl ConnectionIpStream {
    pub fn new(connection: Arc<ConnectionIp>) -> ConnectionIpStream {
        ConnectionIpStream { connection }
    }
}

impl Stream for ConnectionIpStream {
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        // eprintln!("in <ConnectionIpStream as Stream>::poll");
        self.connection.poll_endpoints()
    }
}

#[derive(Debug)]
pub struct ConnectionIpAcceptor {
    connection: Weak<ConnectionIp>,
    server_tcp: Mutex<Incoming>,
}
impl ConnectionIpAcceptor {
    pub fn new(
        connection: Weak<ConnectionIp>,
        addr: Option<SocketAddr>,
    ) -> Result<ConnectionIpAcceptor> {
        let addr = addr.unwrap_or_else(|| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), DEFAULT_PORT)
        });
        let server_tcp = Mutex::new(TcpListener::bind(&addr)?.incoming());
        Ok(ConnectionIpAcceptor {
            connection,
            server_tcp,
        })
    }
}
impl Stream for ConnectionIpAcceptor {
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<Option<()>, Error> {
        let mut incoming = self.server_tcp.lock()?;
        loop {
            let connection = match self.connection.upgrade() {
                Some(c) => c,
                None => return Ok(Async::Ready(None)),
            };
            let socket = match try_ready!(incoming.poll()) {
                Some(s) => s,
                None => return Ok(Async::Ready(None)),
            };
            // OK, we got a new one.
            let endpoints = connection.endpoints();
            tokio::spawn(
                incoming_handshake(socket)
                    .and_then(move |stream| {
                        if let Ok(peer) = stream.peer_addr() {
                            eprintln!("Got connection from {:?}", peer);
                        } else {
                            eprintln!("Got connection from some peer we couldn't identify");
                        }
                        if let Ok(mut epoints) = endpoints.lock() {
                            epoints.push(Some(EndpointIp::new(stream)));
                        }
                        Ok(())
                    })
                    .map_err(|e| {
                        eprintln!("err: {:?}", e);
                    }),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        handler::{HandlerCode, TypedHandler},
        tracker::*,
        Message, StaticSenderName, StaticTypeName, TypeSafeId,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug)]
    struct TrackerHandler {
        flag: Arc<Mutex<bool>>,
    }
    impl TypedHandler for TrackerHandler {
        type Item = PoseReport;
        fn handle_typed(&mut self, msg: &Message<PoseReport>) -> Result<HandlerCode> {
            println!("{:?}", msg);
            let mut flag = self.flag.lock()?;
            *flag = true;
            Ok(HandlerCode::ContinueProcessing)
        }
    }

    #[ignore] // because it requires an external server to be running.
    #[test]
    fn tracker() {
        use crate::async_io::connect_tcp;
        let addr = "127.0.0.1:3883".parse().unwrap();
        let flag = Arc::new(Mutex::new(false));

        connect_tcp(addr)
            .and_then(|stream| -> Result<()> {
                let conn = ConnectionIp::new_client(None, None, stream)?;
                let sender = conn
                    .register_sender(StaticSenderName(b"Tracker0"))
                    .expect("should be able to register sender");
                let handler_handle = conn.add_typed_handler(
                    Box::new(TrackerHandler {
                        flag: Arc::clone(&flag),
                    }),
                    Some(sender),
                )?;
                conn.pack_all_descriptions()?;
                for _ in 0..4 {
                    let _ = conn.poll_endpoints()?;
                }
                conn.remove_handler(handler_handle)
                    .expect("should be able to remove handler");
                Ok(())
            })
            .wait()
            .unwrap();
        assert!(*flag.lock().unwrap() == true);
    }

    #[ignore] // because it requires an external server to be running.
    #[test]
    fn tracker_manual() {
        use crate::async_io::connect_tcp;
        let addr = "127.0.0.1:3883".parse().unwrap();
        let flag = Arc::new(Mutex::new(false));

        connect_tcp(addr)
            .and_then(|stream| {
                let conn = ConnectionIp::new_client(None, None, stream)?;
                let tracker_message_id = conn
                    .register_type(StaticTypeName(b"vrpn_Tracker Pos_Quat"))
                    .expect("should be able to register type");
                let sender = conn
                    .register_sender(StaticSenderName(b"Tracker0"))
                    .expect("should be able to register sender");
                conn.add_handler(
                    Box::new(TrackerHandler {
                        flag: Arc::clone(&flag),
                    }),
                    Some(tracker_message_id),
                    Some(sender),
                )?;
                conn.pack_all_descriptions()?;
                for _ in 0..4 {
                    let _ = conn.poll_endpoints()?;
                }
                Ok(())
                // Ok(future::poll_fn(move || {
                //     eprintln!("polling");
                //     conn.poll_endpoints()
                // })
                // .timeout(Duration::from_secs(4))
                // .map(|_| ()))
            })
            .wait()
            .unwrap();
        assert!(*flag.lock().unwrap() == true);
    }
}
