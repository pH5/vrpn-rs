// Copyright 2018, Collabora, Ltd.
// SPDX-License-Identifier: BSL-1.0
// Author: Ryan A. Pavlik <ryan.pavlik@collabora.com>

// Null tracker server: provides a tracker at Tracker0@localhost
// that just reports the identity transform on a regular basis.

extern crate tokio;
extern crate vrpn;
#[macro_use]
extern crate futures;

use std::{sync::Arc, time::Duration};
use tokio::{prelude::*, timer::Interval};
use vrpn::{
    handler::{HandlerCode, TypedHandler},
    ping,
    prelude::*,
    tracker::PoseReport,
    vrpn_tokio::{
        connect_tcp, connection_ip::ConnectionIpAcceptor, drain_poll_fn, ConnectionIp,
        ConnectionIpStream, Drain, StreamExtras,
    },
    Error, LocalId, Message, Quat, Result, SenderId, Sensor, ServiceFlags, StaticSenderName, Vec3,
};
#[derive(Debug)]
struct ConnectionAndServer {
    connection: Arc<ConnectionIp>,
    // conn_stream: ConnectionIpStream,
    interval: Interval,
    sender: LocalId<SenderId>,
}

impl ConnectionAndServer {
    fn new(connection: Arc<ConnectionIp>) -> Result<ConnectionAndServer> {
        let sender = connection.register_sender(StaticSenderName(b"Tracker0"))?;
        // let conn_stream = ConnectionIpStream::new(Arc::clone(&connection));
        Ok(ConnectionAndServer {
            connection,
            // conn_stream,
            interval: Interval::new_interval(Duration::from_millis(500)),
            sender,
        })
    }
}

impl Future for ConnectionAndServer {
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if drain_poll_fn(|| self.connection.poll_endpoints())?.is_ready() {
            return Ok(Async::Ready(()));
        }
        if self
            .interval
            .poll()
            .map_err(|e| Error::OtherMessage(e.to_string()))?
            .is_ready()
        {
            // OK, send a report.
            let pose = PoseReport {
                sensor: Sensor(0),
                pos: Vec3::new(0.0, 0.0, 0.0),
                quat: Quat::new(1.0, 0.0, 0.0, 0.0),
            };
            self.connection.pack_message_body(
                None,
                self.sender,
                pose,
                ServiceFlags::LOW_LATENCY.into(),
            )?;
        }
        Ok(Async::NotReady)
    }
}

fn main() -> Result<()> {
    // let addr = "127.0.0.1:3883".parse().unwrap();
    let connection = ConnectionIp::new_server(None, None)?;
    let acceptor = ConnectionIpAcceptor::new(Arc::downgrade(&connection), None)?;

    tokio::run(
        ConnectionAndServer::new(connection).unwrap()
            .select(acceptor.drain())
            .map(|((), _)| ())
            .map_err(|e| {
                eprintln!("error {:?}", e);
            })
            // .take_while(|v| v.is_some()),
    );
    // .for_each(|v| {
    //     eprintln!("handled something");
    //     Ok(())
    // })
    // .wait()
    // .unwrap();
    Ok(())
}
