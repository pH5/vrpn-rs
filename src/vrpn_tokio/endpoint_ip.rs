// Copyright 2018, Collabora, Ltd.
// SPDX-License-Identifier: BSL-1.0
// Author: Ryan A. Pavlik <ryan.pavlik@collabora.com>

use bytes::Bytes;
use crate::types::*;
use crate::{
    descriptions::InnerDescription,
    endpoint::*,
    vrpn_tokio::{
        codec::{self, FramedMessageCodec},
        endpoint_channel::{poll_and_dispatch, EndpointChannel},
    },
    Description, Error, GenericMessage, MatchingTable, Message, Result, TranslationTables,
    TypeDispatcher, TypedMessageBody,
};
use futures::sync::mpsc;
use std::{
    ops::DerefMut,
    sync::{Arc, Mutex},
};
use tokio::{
    net::{TcpStream, UdpFramed},
    prelude::*,
};

pub type MessageFramed = codec::MessageFramed<TcpStream>;
pub type MessageFramedUdp = UdpFramed<FramedMessageCodec>;

#[derive(Debug)]
pub struct EndpointIp {
    translation: TranslationTables,
    reliable_channel: Arc<Mutex<EndpointChannel<MessageFramed>>>,
    low_latency_channel: Option<()>,
    system_rx: mpsc::UnboundedReceiver<SystemMessage>,
    system_tx: mpsc::UnboundedSender<SystemMessage>,
}
impl EndpointIp {
    pub(crate) fn new(
        reliable_stream: TcpStream //low_latency_channel: Option<MessageFramedUdp>
    ) -> EndpointIp {
        let framed = codec::apply_message_framing(reliable_stream);
        let (system_tx, system_rx) = mpsc::unbounded();
        EndpointIp {
            translation: TranslationTables::new(),
            reliable_channel: EndpointChannel::new(framed),
            low_latency_channel: None,
            system_tx,
            system_rx,
        }
    }

    pub(crate) fn pack_description<T>(&mut self, local_id: LocalId<T>) -> Result<()>
    where
        T: BaseTypeSafeId,
        InnerDescription<T>: TypedMessageBody,
        TranslationTables: MatchingTable<T>,
    {
        let LocalId(id) = local_id;
        let name = self
            .translation
            .find_by_local_id(local_id)
            .ok_or_else(|| Error::InvalidId(id.get()))
            .and_then(|entry| Ok(entry.name().clone()))?;
        let desc_msg = Message::from(Description::new(id, name));
        self.buffer_message(desc_msg, ClassOfService::from(ServiceFlags::RELIABLE))
            .map(|_| ())
    }

    pub(crate) fn pack_all_descriptions(&mut self) -> Result<()> {
        {
            let mut messages = Vec::new();
            for entry in self.translation.senders.iter() {
                let desc_msg = Message::from(Description::new(
                    entry.local_id().into_id(),
                    entry.name().clone(),
                ));
                messages.push(desc_msg);
            }
            for msg in messages.into_iter() {
                self.buffer_message(msg, ClassOfService::from(ServiceFlags::RELIABLE))?;
            }
        }
        {
            let mut messages = Vec::new();
            for entry in self.translation.types.iter() {
                let desc_msg = Message::from(Description::new(
                    entry.local_id().into_id(),
                    entry.name().clone(),
                ));
                messages.push(desc_msg);
            }
            for msg in messages.into_iter() {
                self.buffer_message(msg, ClassOfService::from(ServiceFlags::RELIABLE))?;
            }
        }
        Ok(())
    }

    pub(crate) fn clear_other_senders_and_types(&mut self) {
        self.translation.clear();
    }

    /// Convert remote sender/type ID to local sender/type ID
    pub(crate) fn map_to_local_id<T>(&self, remote_id: RemoteId<T>) -> Option<LocalId<T>>
    where
        T: BaseTypeSafeId,
        TranslationTables: MatchingTable<T>,
    {
        match self.translation.map_to_local_id(remote_id) {
            Ok(val) => val,
            Err(_) => None,
        }
    }

    pub(crate) fn new_local_id<T, U>(&mut self, name: U, local_id: LocalId<T>) -> Result<()>
    where
        T: BaseTypeSafeIdName + BaseTypeSafeId,
        InnerDescription<T>: TypedMessageBody,
        TranslationTables: MatchingTable<T>,
        U: Into<<T as BaseTypeSafeIdName>::Name>,
    {
        let name: <T as BaseTypeSafeIdName>::Name = name.into();
        let name: Bytes = name.into();
        if self.translation.add_local_id(name, local_id) {
            self.pack_description(local_id)
        } else {
            Ok(())
        }
    }

    pub(crate) fn poll_endpoint(&mut self, dispatcher: &mut TypeDispatcher) -> Poll<(), Error> {
        let channel_arc = Arc::clone(&self.reliable_channel);
        let mut channel = channel_arc
            .lock()
            .map_err(|e| Error::OtherMessage(e.to_string()))?;
        let _ = channel.poll_complete()?;
        let closed = poll_and_dispatch(self, channel.deref_mut(), dispatcher)?.is_ready();

        // todo UDP here.

        // Now, process the messages we sent ourself.
        while let Async::Ready(Some(msg)) = self.system_rx.poll().map_err(|()| {
            Error::OtherMessage(String::from(
                "error when polling system change message channel",
            ))
        })? {
            match msg {
                SystemMessage::SenderDescription(desc) => {
                    let local_id = dispatcher
                        .register_sender(SenderName(desc.name.clone()))?
                        .get();
                    eprintln!(
                        "Registering sender {:?}: local {:?} = remote {:?}",
                        desc.name, local_id, desc.which
                    );
                    let _ = self.translation.add_remote_entry(
                        desc.name,
                        RemoteId(desc.which),
                        LocalId(local_id),
                    )?;
                }
                SystemMessage::TypeDescription(desc) => {
                    let local_id = dispatcher.register_type(TypeName(desc.name.clone()))?.get();
                    eprintln!(
                        "Registering type {:?}: local {:?} = remote {:?}",
                        desc.name, local_id, desc.which
                    );
                    let _ = self.translation.add_remote_entry(
                        desc.name,
                        RemoteId(desc.which),
                        LocalId(local_id),
                    )?;
                }
                SystemMessage::UdpDescription(desc) => {
                    eprintln!("UdpDescription: {:?}", desc);
                }
                SystemMessage::LogDescription(desc) => {
                    eprintln!("LogDescription: {:?}", desc);
                }
                SystemMessage::DisconnectMessage => {
                    eprintln!("DesconnectMessage");
                }
            }
        }

        if closed {
            Ok(Async::Ready(()))
        } else {
            Ok(Async::NotReady)
        }
    }
}

impl Endpoint for EndpointIp {
    fn send_system_change(&self, message: SystemMessage) -> Result<()> {
        println!("send_system_change {:?}", message);
        self.system_tx
            .unbounded_send(message)
            .map_err(|e| Error::OtherMessage(e.to_string()))?;
        Ok(())
    }

    fn buffer_generic_message(&mut self, msg: GenericMessage, class: ClassOfService) -> Result<()> {
        if class.contains(ServiceFlags::RELIABLE) || self.low_latency_channel.is_none() {
            // We either need reliable, or don't have low-latency
            let mut channel = self
                .reliable_channel
                .lock()
                .map_err(|e| Error::OtherMessage(e.to_string()))?;
            match channel
                .start_send(msg)
                .map_err(|e| Error::OtherMessage(e.to_string()))?
            {
                AsyncSink::Ready => Ok(()),
                AsyncSink::NotReady(_) => Err(Error::OtherMessage(String::from(
                    "Didn't have room in send buffer",
                ))),
            }
        } else {
            // have and can use low-latency
            unimplemented!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vrpn_tokio::connect::connect_tcp;
    #[test]
    fn make_endpoint() {
        let addr = "127.0.0.1:3883".parse().unwrap();
        let _ = connect_tcp(addr)
            .and_then(|stream| {
                let ep = EndpointIp::new(stream);
                for _i in 0..4 {
                    let _ = ep
                        .reliable_channel
                        .lock()
                        .unwrap()
                        .poll()
                        .unwrap()
                        .map(|msg| {
                            eprintln!("Received message {:?}", msg);
                            msg
                        });
                }
                Ok(())
            })
            .wait()
            .unwrap();
    }
    #[test]
    fn run_endpoint() {
        let addr = "127.0.0.1:3883".parse().unwrap();
        let _ = connect_tcp(addr)
            .and_then(|stream| {
                let mut ep = EndpointIp::new(stream);
                let mut disp = TypeDispatcher::new();
                for _i in 0..4 {
                    let _ = ep.poll_endpoint(&mut disp).unwrap();
                }
                Ok(())
            })
            .wait()
            .unwrap();
    }
}